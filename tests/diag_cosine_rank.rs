//! One-off diagnostic (not a regression test): measures pure cosine-similarity
//! ranks of LoCoMo gold evidence turns to separate embedding-recall failures
//! from engine-ranking failures. Run with:
//! `cargo test --features embed --test diag_cosine_rank -- --ignored --nocapture`
#![cfg(feature = "embed")]

#[path = "../benches/eval_common/mod.rs"]
mod eval_common;

use std::path::Path;

use anamnesis::FastEmbedProvider;
use anamnesis::embedding::EmbeddingProvider;
use eval_common::real_bench::dataset::{BenchDatasetName, load_benchmark_dataset};

const BGE_QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn rank_of(golds: &[usize], query: &[f32], turns: &[Vec<f32>]) -> Vec<(usize, usize, f64)> {
    let mut scored: Vec<(usize, f64)> = turns
        .iter()
        .enumerate()
        .map(|(i, e)| (i, cosine(query, e)))
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    golds
        .iter()
        .map(|gold| {
            let pos = scored.iter().position(|(i, _)| i == gold).unwrap();
            (*gold, pos + 1, scored[pos].1)
        })
        .collect()
}

/// Reproduces the engine's vector-candidate stage over the mixed view pool
/// (speaker view + window view per turn) and reports where gold turns' views
/// land, to audit which channel a final hit came from.
#[test]
#[ignore = "diagnostic only"]
fn diag_mixed_view_vector_candidates() {
    let data_dir = Path::new("benches/eval/data");
    let loaded = load_benchmark_dataset(BenchDatasetName::Locomo, data_dir, Some(1))
        .expect("load locomo sample 0");
    let provider = FastEmbedProvider::new().expect("init bge");

    let turns: Vec<_> = loaded
        .sessions
        .iter()
        .flat_map(|s| s.turns.iter())
        .filter(|t| !t.content.trim().is_empty())
        .collect();
    let spk_texts: Vec<String> = turns
        .iter()
        .map(|t| format!("{}: {}", t.speaker, t.content))
        .collect();
    let win_texts: Vec<String> = turns
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut parts = Vec::new();
            if i > 0 && turns[i - 1].session_id == t.session_id {
                parts.push(format!(
                    "{}: {}",
                    turns[i - 1].speaker,
                    turns[i - 1].content
                ));
            }
            parts.push(format!("{}: {}", t.speaker, t.content));
            if i + 1 < turns.len() && turns[i + 1].session_id == t.session_id {
                parts.push(format!(
                    "{}: {}",
                    turns[i + 1].speaker,
                    turns[i + 1].content
                ));
            }
            parts.join("\n")
        })
        .collect();
    let embed = |texts: &[String]| -> Vec<Vec<f32>> {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        provider.embed(&refs).expect("embed")
    };
    let spk_emb = embed(&spk_texts);
    let win_emb = embed(&win_texts);

    // Mixed pool labelled (turn_index, view).
    let mut pool: Vec<(usize, &str, &Vec<f32>)> = Vec::new();
    for (i, e) in spk_emb.iter().enumerate() {
        pool.push((i, "spk", e));
    }
    for (i, e) in win_emb.iter().enumerate() {
        pool.push((i, "win", e));
    }

    for q in loaded.questions.iter().skip(2).take(3) {
        let qe = embed(std::slice::from_ref(&q.question));
        let mut scored: Vec<(usize, &str, f64)> = pool
            .iter()
            .map(|(i, view, e)| (*i, *view, cosine(&qe[0], e)))
            .collect();
        scored.sort_by(|a, b| b.2.total_cmp(&a.2));
        println!("\n=== {} {}", q.question_id, q.question);
        println!("gold: {:?}", q.gold.evidence_turn_ids);
        println!("vector top-20 (engine-equivalent candidate set):");
        for (rank, (i, view, score)) in scored.iter().take(20).enumerate() {
            let tid = turns[*i].raw_turn_id.as_deref().unwrap_or("?");
            let is_gold = q.gold.evidence_turn_ids.iter().any(|g| g == tid);
            println!(
                "  {:>2}. {tid:<8} {view} cos={score:.4}{}",
                rank + 1,
                if is_gold { "  <-- GOLD VIEW" } else { "" }
            );
        }
        for gid in &q.gold.evidence_turn_ids {
            for view in ["spk", "win"] {
                if let Some(pos) = scored.iter().position(|(i, v, _)| {
                    *v == view && turns[*i].raw_turn_id.as_deref() == Some(gid)
                }) {
                    println!("  gold {gid} {view}-view mixed rank: {}", pos + 1);
                }
            }
        }
    }
}

#[test]
#[ignore = "diagnostic only"]
fn diag_locomo_cosine_ranks() {
    let data_dir = Path::new("benches/eval/data");
    let loaded = load_benchmark_dataset(BenchDatasetName::Locomo, data_dir, Some(1))
        .expect("load locomo sample 0");
    let provider = FastEmbedProvider::new().expect("init bge");

    let turns: Vec<_> = loaded
        .sessions
        .iter()
        .flat_map(|s| s.turns.iter())
        .filter(|t| !t.content.trim().is_empty())
        .collect();
    println!("turns: {}", turns.len());

    let raw_texts: Vec<String> = turns.iter().map(|t| t.content.clone()).collect();
    let speaker_texts: Vec<String> = turns
        .iter()
        .map(|t| format!("{}: {}", t.speaker, t.content))
        .collect();

    let embed = |texts: &[String]| -> Vec<Vec<f32>> {
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        provider.embed(&refs).expect("embed")
    };
    // ±1-turn context window, speaker-prefixed, within the same session.
    let window_texts: Vec<String> = turns
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let mut parts = Vec::new();
            if i > 0 && turns[i - 1].session_id == t.session_id {
                parts.push(format!(
                    "{}: {}",
                    turns[i - 1].speaker,
                    turns[i - 1].content
                ));
            }
            parts.push(format!("{}: {}", t.speaker, t.content));
            if i + 1 < turns.len() && turns[i + 1].session_id == t.session_id {
                parts.push(format!(
                    "{}: {}",
                    turns[i + 1].speaker,
                    turns[i + 1].content
                ));
            }
            parts.join("\n")
        })
        .collect();

    let raw_emb = embed(&raw_texts);
    let spk_emb = embed(&speaker_texts);
    let win_emb = embed(&window_texts);

    // First 2 questions were warmup in the smoke run; evaluate the next 3.
    for q in loaded.questions.iter().skip(2).take(3) {
        let golds: Vec<usize> = q
            .gold
            .evidence_turn_ids
            .iter()
            .filter_map(|gid| {
                turns
                    .iter()
                    .position(|t| t.raw_turn_id.as_deref() == Some(gid.as_str()))
            })
            .collect();
        let plain_q = embed(std::slice::from_ref(&q.question));
        let prefixed = format!("{BGE_QUERY_PREFIX}{}", q.question);
        let prefix_q = embed(std::slice::from_ref(&prefixed));

        println!(
            "\n=== {} [{}] {}",
            q.question_id, q.question_type, q.question
        );
        println!("gold: {:?}", q.gold.evidence_turn_ids);
        for (label, qe, te) in [
            ("raw-q / raw-turn      (current)", &plain_q[0], &raw_emb),
            ("bge-q / raw-turn      ", &prefix_q[0], &raw_emb),
            ("raw-q / speaker-turn  ", &plain_q[0], &spk_emb),
            ("bge-q / speaker-turn  ", &prefix_q[0], &spk_emb),
            ("raw-q / window-turn   ", &plain_q[0], &win_emb),
            ("bge-q / window-turn   ", &prefix_q[0], &win_emb),
        ] {
            for (gold, rank, score) in rank_of(&golds, qe, te) {
                println!(
                    "  {label} | gold {} -> cosine rank {rank}/{} (cos={score:.4})",
                    turns[gold].raw_turn_id.as_deref().unwrap_or("?"),
                    turns.len()
                );
            }
        }
    }
}
