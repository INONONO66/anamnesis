//! LongMemEval ablation matrix measurement.
//!
//! Runs 6+ named engine configurations and records accuracy for each.
//!
//! Usage:
//!   cargo bench --bench longmemeval-ablation -- \
//!     --output ablation.json --limit 50 --judge mock --combinations all

#[allow(dead_code, unused_imports)]
#[path = "judge.rs"]
mod judge;

#[allow(dead_code)]
#[path = "loader.rs"]
mod loader;

use judge::{Judge, MockJudge};
use loader::{ConversationTurn, EvalQuestion, Session, load_questions, load_sessions};

use std::path::Path;

use anamnesis::api::{CrystallizeRequest, DecayModel, EnergyModel, EngineConfig, SpreadingModel};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, NodeId, Timestamp};
use anamnesis::query::{Query, QueryConfig};
use anamnesis::storage::StorageAdapter;
use anamnesis::{Engine, Observation};

// ---------------------------------------------------------------------------
// CLI parsing
// ---------------------------------------------------------------------------

struct Args {
    output: String,
    limit: usize,
    #[allow(dead_code)]
    judge: String,
    combinations: String,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut output = ".sisyphus/evidence/cycle-4/ablation.json".to_string();
    let mut limit = 50usize;
    let mut judge = "mock".to_string();
    let mut combinations = "all".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--output" if i + 1 < args.len() => {
                output = args[i + 1].clone();
                i += 2;
            }
            "--limit" if i + 1 < args.len() => {
                limit = args[i + 1].parse().unwrap_or(50);
                i += 2;
            }
            "--judge" if i + 1 < args.len() => {
                judge = args[i + 1].clone();
                i += 2;
            }
            "--combinations" if i + 1 < args.len() => {
                combinations = args[i + 1].clone();
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    Args {
        output,
        limit,
        judge,
        combinations,
    }
}

// ---------------------------------------------------------------------------
// Combination definitions
// ---------------------------------------------------------------------------

struct Combination {
    name: &'static str,
    decay_model: DecayModel,
    energy_model: EnergyModel,
    spreading_model: SpreadingModel,
    crystallize_enabled: bool,
}

fn all_combinations() -> Vec<Combination> {
    vec![
        Combination {
            name: "baseline-c0",
            decay_model: DecayModel::Exponential,
            energy_model: EnergyModel::WeightedSum,
            spreading_model: SpreadingModel::PriorityQueueBfs,
            crystallize_enabled: false,
        },
        Combination {
            name: "cycle-1",
            decay_model: DecayModel::PowerLaw,
            energy_model: EnergyModel::WeightedSum,
            spreading_model: SpreadingModel::PriorityQueueBfs,
            crystallize_enabled: false,
        },
        Combination {
            name: "hopfield",
            decay_model: DecayModel::PowerLaw,
            energy_model: EnergyModel::Hopfield,
            spreading_model: SpreadingModel::PriorityQueueBfs,
            crystallize_enabled: false,
        },
        Combination {
            name: "rwr",
            decay_model: DecayModel::PowerLaw,
            energy_model: EnergyModel::WeightedSum,
            spreading_model: SpreadingModel::RandomWalkRestart,
            crystallize_enabled: false,
        },
        Combination {
            name: "full",
            decay_model: DecayModel::PowerLaw,
            energy_model: EnergyModel::Hopfield,
            spreading_model: SpreadingModel::RandomWalkRestart,
            crystallize_enabled: false,
        },
        Combination {
            name: "full+xtl",
            decay_model: DecayModel::PowerLaw,
            energy_model: EnergyModel::Hopfield,
            spreading_model: SpreadingModel::RandomWalkRestart,
            crystallize_enabled: true,
        },
    ]
}

fn filter_combinations(all: Vec<Combination>, filter: &str) -> Vec<Combination> {
    if filter == "all" {
        return all;
    }
    let requested: Vec<&str> = filter.split(',').map(|s| s.trim()).collect();
    all.into_iter()
        .filter(|c| requested.contains(&c.name))
        .collect()
}

// ---------------------------------------------------------------------------
// Engine helpers
// ---------------------------------------------------------------------------

fn make_engine(combo: &Combination) -> Engine {
    let mut config = EngineConfig::default();
    config.decay_model = combo.decay_model;
    config.energy_model = combo.energy_model;
    config.spreading_model = combo.spreading_model;
    Engine::with_config(config)
}

fn ingest_session(engine: &mut Engine, session: &Session) -> Vec<NodeId> {
    let mut ids = Vec::new();
    for (i, turn) in session.turns.iter().enumerate() {
        let obs = Observation {
            name: format!("{}: turn {}", session.session_id, i),
            summary: None,
            content: turn.content.clone(),
            embedding: None,
            confidence: 0.9,
            node_type: KnowledgeType::Episodic,
            entity_tags: vec![session.session_id.clone()],
            origin: Origin {
                agent_id: turn.speaker.clone(),
                session_id: session.session_id.clone(),
                scope: anamnesis::graph::ScopePath::universal(),
                confidence: 0.9,
            },
            timestamp: Timestamp(i as u64 * 1000),
        };
        if let Ok(result) = engine.ingest(obs) {
            match result {
                anamnesis::IngestResult::Created(new_ids) => ids.extend(new_ids),
                anamnesis::IngestResult::Reinforced { existing_id, .. } => ids.push(existing_id),
            }
        }
    }
    ids
}

fn try_crystallize(engine: &mut Engine, node_ids: &[NodeId]) {
    if node_ids.len() < 2 {
        return;
    }
    let source_ids: Vec<NodeId> = node_ids.iter().take(2).copied().collect();
    let request = CrystallizeRequest {
        name: "ablation-crystal".to_string(),
        summary: Some("Ablation benchmark crystallization".to_string()),
        content: "Crystallized from ablation benchmark session".to_string(),
        embedding: None,
        source_ids,
        source_relevances: Some(vec![0.8, 0.6]),
        node_type: KnowledgeType::Semantic,
        confidence: 0.85,
        origin: Origin {
            agent_id: "ablation-bench".to_string(),
            session_id: "ablation-session".to_string(),
            scope: anamnesis::graph::ScopePath::universal(),
            confidence: 0.85,
        },
        entity_tags: vec![],
        timestamp: Timestamp(99_000),
    };
    let _ = engine.crystallize(request);
}

fn answer_question(engine: &Engine, question: &EvalQuestion) -> String {
    let storage = engine.graph().storage();
    let session_nodes = storage.nodes_by_entity_tag(&question.session_id);
    if session_nodes.is_empty() {
        return String::new();
    }
    let seed = session_nodes[0];

    let q = Query::Associative { seed, budget: 50 };
    let config = QueryConfig::default();
    match engine.query(&q, &config) {
        Ok(pkg) => {
            let best = pkg
                .knowledge
                .iter()
                .chain(pkg.memories.iter())
                .chain(pkg.identity.iter())
                .max_by(|a, b| {
                    a.relevance
                        .partial_cmp(&b.relevance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            best.and_then(|f| f.content.clone()).unwrap_or_default()
        }
        Err(_) => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Mock data
// ---------------------------------------------------------------------------

fn mock_sessions() -> Vec<Session> {
    vec![Session {
        session_id: "mock-session-1".to_string(),
        turns: vec![
            ConversationTurn {
                speaker: "user".to_string(),
                content: "My name is Alice and I work at Acme Corp.".to_string(),
            },
            ConversationTurn {
                speaker: "assistant".to_string(),
                content: "Nice to meet you, Alice!".to_string(),
            },
        ],
    }]
}

fn mock_questions() -> Vec<EvalQuestion> {
    vec![EvalQuestion {
        question_id: "q1".to_string(),
        session_id: "mock-session-1".to_string(),
        question: "What is the user's name?".to_string(),
        expected_answer: "Alice".to_string(),
        question_type: "single-session-user".to_string(),
    }]
}

// ---------------------------------------------------------------------------
// Evaluation runner
// ---------------------------------------------------------------------------

struct CombinationResult {
    name: &'static str,
    total_questions: usize,
    correct: usize,
    accuracy: f64,
    decay_model: &'static str,
    energy_model: &'static str,
    spreading_model: &'static str,
    crystallize_enabled: bool,
}

fn decay_model_str(m: DecayModel) -> &'static str {
    match m {
        DecayModel::Exponential => "exponential",
        DecayModel::PowerLaw => "power-law",
    }
}

fn energy_model_str(m: EnergyModel) -> &'static str {
    match m {
        EnergyModel::WeightedSum => "weighted-sum",
        EnergyModel::Hopfield => "hopfield",
    }
}

fn spreading_model_str(m: SpreadingModel) -> &'static str {
    match m {
        SpreadingModel::PriorityQueueBfs => "priority-queue-bfs",
        SpreadingModel::NormalizedPriorityQueueBfs => "normalized-priority-queue-bfs",
        SpreadingModel::RandomWalkRestart => "random-walk-restart",
    }
}

fn run_combination(
    combo: &Combination,
    sessions: &[Session],
    questions: &[EvalQuestion],
    session_limit: usize,
    question_limit: usize,
    judge: &MockJudge,
) -> CombinationResult {
    let mut engine = make_engine(combo);

    let mut all_node_ids: Vec<NodeId> = Vec::new();
    for session in sessions.iter().take(session_limit) {
        let ids = ingest_session(&mut engine, session);
        all_node_ids.extend(ids);
    }

    if combo.crystallize_enabled {
        try_crystallize(&mut engine, &all_node_ids);
    }

    let mut total = 0usize;
    let mut correct = 0usize;

    for question in questions.iter().take(question_limit) {
        let actual = answer_question(&engine, question);
        let result = judge.evaluate(&question.question, &question.expected_answer, &actual);
        total += 1;
        if result.correct {
            correct += 1;
        }
    }

    let accuracy = if total > 0 {
        correct as f64 / total as f64
    } else {
        0.0
    };

    CombinationResult {
        name: combo.name,
        total_questions: total,
        correct,
        accuracy,
        decay_model: decay_model_str(combo.decay_model),
        energy_model: energy_model_str(combo.energy_model),
        spreading_model: spreading_model_str(combo.spreading_model),
        crystallize_enabled: combo.crystallize_enabled,
    }
}

// ---------------------------------------------------------------------------
// JSON output
// ---------------------------------------------------------------------------

fn escape_json_str(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn write_json(path: &str, results: &[CombinationResult]) {
    let mut entries = String::new();
    for (i, r) in results.iter().enumerate() {
        if i > 0 {
            entries.push_str(",\n");
        }
        entries.push_str(&format!(
            r#"    {{
      "name": "{}",
      "accuracy": {:.6},
      "total_questions": {},
      "correct": {},
      "decay_model": "{}",
      "energy_model": "{}",
      "spreading_model": "{}",
      "crystallize_enabled": {}
    }}"#,
            escape_json_str(r.name),
            r.accuracy,
            r.total_questions,
            r.correct,
            escape_json_str(r.decay_model),
            escape_json_str(r.energy_model),
            escape_json_str(r.spreading_model),
            r.crystallize_enabled,
        ));
    }

    let json = format!(
        r#"{{
  "combinations": [
{}
  ]
}}"#,
        entries
    );

    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, json).expect("failed to write output JSON");
    eprintln!("Results written to: {path}");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args = parse_args();
    eprintln!("LongMemEval Ablation Matrix");
    eprintln!("  Output: {}", args.output);
    eprintln!("  Limit: {}", args.limit);
    eprintln!("  Judge: {}", args.judge);
    eprintln!("  Combinations: {}", args.combinations);
    eprintln!();

    let data_dir = Path::new("benches/longmemeval/data");
    let sessions = match load_sessions(data_dir) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            eprintln!("Dataset loaded but empty, using mock data.");
            mock_sessions()
        }
        Err(e) => {
            eprintln!("Dataset not available: {e}");
            eprintln!("Using mock data for ablation measurement.");
            mock_sessions()
        }
    };

    let questions = match load_questions(data_dir) {
        Ok(q) if !q.is_empty() => q,
        _ => mock_questions(),
    };

    if args.judge != "mock" {
        eprintln!(
            "Warning: only 'mock' judge is supported in Rust benchmark, got '{}'",
            args.judge
        );
    }
    let judge = MockJudge;

    let combos = filter_combinations(all_combinations(), &args.combinations);
    if combos.is_empty() {
        eprintln!(
            "No matching combinations found for filter: {}",
            args.combinations
        );
        std::process::exit(1);
    }

    let session_limit = args.limit.min(sessions.len());
    let question_limit = args.limit.min(questions.len());

    eprintln!(
        "Running {} combinations (sessions={session_limit}, questions={question_limit})...",
        combos.len()
    );
    eprintln!();

    let mut results: Vec<CombinationResult> = Vec::new();

    for combo in &combos {
        eprintln!(
            "  [{}] decay={} energy={} spreading={} xtl={}",
            combo.name,
            decay_model_str(combo.decay_model),
            energy_model_str(combo.energy_model),
            spreading_model_str(combo.spreading_model),
            combo.crystallize_enabled,
        );

        let result = run_combination(
            combo,
            &sessions,
            &questions,
            session_limit,
            question_limit,
            &judge,
        );

        eprintln!(
            "    accuracy={:.2}% ({}/{})",
            result.accuracy * 100.0,
            result.correct,
            result.total_questions
        );
        results.push(result);
    }

    eprintln!();
    write_json(&args.output, &results);

    eprintln!();
    eprintln!("Summary:");
    for r in &results {
        eprintln!("  {:16} {:.2}%", r.name, r.accuracy * 100.0);
    }
}
