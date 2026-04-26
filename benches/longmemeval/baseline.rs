//! LongMemEval baseline measurement.
//!
//! Measures the accuracy of the Anamnesis engine on the LongMemEval benchmark.
//!
//! Usage:
//!   cargo bench --bench longmemeval-baseline -- \
//!     --output results.json --limit 50 --judge mock --decay exponential

#[allow(dead_code, unused_imports)]
#[path = "judge.rs"]
mod judge;

#[allow(dead_code)]
#[path = "loader.rs"]
mod loader;

use judge::{Judge, MockJudge};
use loader::{ConversationTurn, EvalQuestion, Session, load_questions, load_sessions};

use std::path::Path;

use anamnesis::Engine;
use anamnesis::api::{DecayModel, EngineConfig, Observation};
use anamnesis::graph::node::Origin;
use anamnesis::graph::{KnowledgeType, Timestamp};
use anamnesis::query::{Query, QueryConfig};
use anamnesis::storage::StorageAdapter;

struct Args {
    output: String,
    limit: usize,
    #[allow(dead_code)]
    judge: String,
    decay: String,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().collect();
    let mut output = "longmemeval-baseline.json".to_string();
    let mut limit = 50usize;
    let mut judge = "mock".to_string();
    let mut decay = "exponential".to_string();

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
            "--decay" if i + 1 < args.len() => {
                decay = args[i + 1].clone();
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
        decay,
    }
}

fn make_engine(decay: &str) -> Engine {
    let decay_model = match decay {
        "power-law" | "powerlaw" => DecayModel::PowerLaw,
        _ => DecayModel::Exponential,
    };
    Engine::with_config(EngineConfig {
        decay_model,
        ..EngineConfig::default()
    })
}

fn ingest_session(engine: &mut Engine, session: &Session) {
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
                project_id: None,
                confidence: 0.9,
            },
            timestamp: Timestamp(i as u64 * 1000),
        };
        let _ = engine.ingest(obs);
    }
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

fn write_json(path: &str, total: usize, correct: usize, decay: &str) {
    let accuracy = if total > 0 {
        correct as f64 / total as f64
    } else {
        0.0
    };
    let json = format!(
        r#"{{
  "total_questions": {total},
  "correct": {correct},
  "accuracy": {accuracy:.6},
  "decay_mode": "{decay}"
}}"#
    );
    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, json).expect("failed to write output JSON");
    eprintln!("Results written to: {path}");
    eprintln!("Accuracy: {:.2}% ({correct}/{total})", accuracy * 100.0);
}

fn write_baseline_report(output_path: &str, total: usize, correct: usize, decay: &str) {
    let accuracy = if total > 0 {
        correct as f64 / total as f64
    } else {
        0.0
    };
    let accuracy_pct = accuracy * 100.0;
    let report = format!(
        "# Cycle 1 Baseline Measurement\n\n\
         ## Results\n\n\
         | Metric | Value |\n\
         |--------|-------|\n\
         | Total Questions | {total} |\n\
         | Correct | {correct} |\n\
         | Accuracy | {accuracy_pct:.2}% |\n\
         | Decay Mode | {decay} |\n\
         | Output | {output_path} |\n",
    );
    let _ = std::fs::create_dir_all("docs");
    std::fs::write("docs/cycle-1-baseline.local.md", &report)
        .expect("failed to write baseline report");
    eprintln!("Baseline report: docs/cycle-1-baseline.local.md");
}

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

fn main() {
    let args = parse_args();
    eprintln!("LongMemEval Baseline Measurement");
    eprintln!("  Output: {}", args.output);
    eprintln!("  Limit: {}", args.limit);
    eprintln!("  Judge: {}", args.judge);
    eprintln!("  Decay: {}", args.decay);
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
            eprintln!("Using mock data for baseline measurement.");
            mock_sessions()
        }
    };

    let questions = match load_questions(data_dir) {
        Ok(q) if !q.is_empty() => q,
        _ => mock_questions(),
    };

    let judge = MockJudge;
    let mut engine = make_engine(&args.decay);

    let session_limit = args.limit.min(sessions.len());
    eprintln!("Ingesting {session_limit} sessions...");
    for session in sessions.iter().take(session_limit) {
        ingest_session(&mut engine, session);
    }

    let question_limit = args.limit.min(questions.len());
    eprintln!("Evaluating {question_limit} questions...");
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

    write_json(&args.output, total, correct, &args.decay);
    write_baseline_report(&args.output, total, correct, &args.decay);
}
