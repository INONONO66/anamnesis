use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnifiedSession {
    pub session_id: String,
    pub turns: Vec<Turn>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnifiedQuestion {
    pub question_id: String,
    pub question: String,
    pub expected_answer: String,
    pub question_type: String,
    pub session_ids: Vec<String>,
}

#[derive(Debug)]
pub enum DatasetError {
    NotFound { path: String, hint: String },
    ParseError(String),
    IoError(String),
}

pub trait Dataset {
    fn name(&self) -> &str;
    fn load(
        &self,
        path: &Path,
    ) -> Result<(Vec<UnifiedSession>, Vec<UnifiedQuestion>), DatasetError>;
}

#[derive(Debug, Default, Clone)]
pub struct LoCoMoLoader;

impl Dataset for LoCoMoLoader {
    fn name(&self) -> &str {
        "locomo"
    }

    fn load(
        &self,
        path: &Path,
    ) -> Result<(Vec<UnifiedSession>, Vec<UnifiedQuestion>), DatasetError> {
        let file_path = path.join("locomo").join("locomo10.json");
        if !file_path.exists() {
            return Err(DatasetError::NotFound {
                path: file_path.display().to_string(),
                hint: "Download with: cargo bench --bench download_datasets -- --dataset locomo"
                    .to_string(),
            });
        }

        let content = std::fs::read_to_string(&file_path)
            .map_err(|e| DatasetError::IoError(e.to_string()))?;
        let samples: Vec<serde_json::Value> =
            serde_json::from_str(&content).map_err(|e| DatasetError::ParseError(e.to_string()))?;

        let mut sessions = Vec::new();
        let mut questions = Vec::new();

        for (sample_idx, sample) in samples.iter().enumerate() {
            let mut session_ids = Vec::new();
            let mut key_idx = 1;
            loop {
                let key = format!("session_{key_idx}");
                if let Some(session_data) = sample.get(&key) {
                    let turns = parse_locomo_turns(session_data)?;
                    let session_id = format!("locomo-{sample_idx}-{key}");
                    sessions.push(UnifiedSession {
                        session_id: session_id.clone(),
                        turns,
                    });
                    session_ids.push(session_id);
                    key_idx += 1;
                } else {
                    break;
                }
            }

            if let Some(qa_array) = sample.get("qa").and_then(|v| v.as_array()) {
                for (qa_idx, qa) in qa_array.iter().enumerate() {
                    let question = qa
                        .get("question")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let expected_answer = qa
                        .get("answer")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let category = qa.get("category").and_then(|v| v.as_u64()).unwrap_or(1);

                    questions.push(UnifiedQuestion {
                        question_id: format!("locomo-{sample_idx}-qa-{qa_idx}"),
                        question,
                        expected_answer,
                        question_type: locomo_category_to_type(category),
                        session_ids: session_ids.clone(),
                    });
                }
            }
        }

        Ok((sessions, questions))
    }
}

fn parse_locomo_turns(session_data: &serde_json::Value) -> Result<Vec<Turn>, DatasetError> {
    let turns_array = session_data
        .as_array()
        .ok_or_else(|| DatasetError::ParseError("session is not an array".to_string()))?;

    let turns = turns_array
        .iter()
        .map(|turn| {
            let speaker = turn
                .get("speaker")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let content = turn
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let role = if speaker == "speaker_a" {
                "user".to_string()
            } else {
                "assistant".to_string()
            };
            Turn { role, content }
        })
        .collect();

    Ok(turns)
}

fn locomo_category_to_type(category: u64) -> String {
    match category {
        1 => "single-hop",
        2 => "multi-hop",
        3 => "temporal",
        4 => "world-knowledge",
        5 => "adversarial",
        _ => "unknown",
    }
    .to_string()
}

#[derive(Debug, Default, Clone)]
pub struct LongMemEvalLoader;

impl Dataset for LongMemEvalLoader {
    fn name(&self) -> &str {
        "longmemeval"
    }

    fn load(
        &self,
        path: &Path,
    ) -> Result<(Vec<UnifiedSession>, Vec<UnifiedQuestion>), DatasetError> {
        let file_path = path.join("longmemeval").join("longmemeval_s.json");
        if !file_path.exists() {
            return Err(DatasetError::NotFound {
                path: file_path.display().to_string(),
                hint:
                    "Download with: cargo bench --bench download_datasets -- --dataset longmemeval"
                        .to_string(),
            });
        }

        let content = std::fs::read_to_string(&file_path)
            .map_err(|e| DatasetError::IoError(e.to_string()))?;
        let instances: Vec<serde_json::Value> =
            serde_json::from_str(&content).map_err(|e| DatasetError::ParseError(e.to_string()))?;

        let mut sessions = Vec::new();
        let mut questions = Vec::new();

        for instance in &instances {
            let question_id = instance
                .get("question_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let question = instance
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let expected_answer = instance
                .get("answer")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let question_type = instance
                .get("question_type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let mut session_ids = Vec::new();
            if let Some(haystack) = instance.get("haystack_sessions").and_then(|v| v.as_array()) {
                if haystack.is_empty() {
                    continue;
                }

                for (sess_idx, session_data) in haystack.iter().enumerate() {
                    let session_id = format!("lme-{question_id}-{sess_idx}");
                    let turns = if let Some(turns_array) = session_data.as_array() {
                        turns_array
                            .iter()
                            .map(|turn| {
                                let role = turn
                                    .get("role")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("user")
                                    .to_string();
                                let content = turn
                                    .get("content")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                Turn { role, content }
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    sessions.push(UnifiedSession {
                        session_id: session_id.clone(),
                        turns,
                    });
                    session_ids.push(session_id);
                }
            }

            questions.push(UnifiedQuestion {
                question_id,
                question,
                expected_answer,
                question_type,
                session_ids,
            });
        }

        Ok((sessions, questions))
    }
}

#[derive(Debug, Default, Clone)]
pub struct ConvoMemLoader;

impl Dataset for ConvoMemLoader {
    fn name(&self) -> &str {
        "convomem"
    }

    fn load(
        &self,
        path: &Path,
    ) -> Result<(Vec<UnifiedSession>, Vec<UnifiedQuestion>), DatasetError> {
        let convomem_path = path.join("convomem");
        let scan_path = if convomem_path.is_dir() {
            &convomem_path
        } else {
            path
        };
        if !scan_path.exists() {
            return Err(DatasetError::NotFound {
                path: scan_path.display().to_string(),
                hint: "Download with: cargo bench --bench download_datasets -- --dataset convomem"
                    .to_string(),
            });
        }

        let entries =
            std::fs::read_dir(scan_path).map_err(|e| DatasetError::IoError(e.to_string()))?;
        let mut all_sessions = Vec::new();
        let mut all_questions = Vec::new();
        let mut found_any = false;

        for entry in entries.flatten() {
            let category_path = entry.path();
            if !category_path.is_dir() {
                continue;
            }

            let batch_file = category_path.join("1_evidence").join("batched_000.json");
            if !batch_file.exists() {
                continue;
            }

            found_any = true;
            let category = category_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            let content = std::fs::read_to_string(&batch_file)
                .map_err(|e| DatasetError::IoError(e.to_string()))?;
            let data: serde_json::Value = serde_json::from_str(&content)
                .map_err(|e| DatasetError::ParseError(e.to_string()))?;

            if let Some(items) = data.get("evidence_items").and_then(|v| v.as_array()) {
                for (item_idx, item) in items.iter().enumerate() {
                    let question = item
                        .get("question")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let answer = item
                        .get("answer")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut session_ids = Vec::new();

                    if let Some(convos) = item.get("conversations").and_then(|v| v.as_array()) {
                        for (conv_idx, conv) in convos.iter().enumerate() {
                            let session_id = format!("convomem-{category}-{item_idx}-{conv_idx}");
                            let turns = if let Some(messages) =
                                conv.get("messages").and_then(|v| v.as_array())
                            {
                                messages
                                    .iter()
                                    .map(|msg| {
                                        let speaker = msg
                                            .get("speaker")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("user");
                                        let content = msg
                                            .get("text")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        let role = if speaker == "user" {
                                            "user".to_string()
                                        } else {
                                            "assistant".to_string()
                                        };
                                        Turn { role, content }
                                    })
                                    .collect()
                            } else {
                                Vec::new()
                            };
                            all_sessions.push(UnifiedSession {
                                session_id: session_id.clone(),
                                turns,
                            });
                            session_ids.push(session_id);
                        }
                    }

                    all_questions.push(UnifiedQuestion {
                        question_id: format!("convomem-{category}-{item_idx}"),
                        question,
                        expected_answer: answer,
                        question_type: category.clone(),
                        session_ids,
                    });
                }
            }
        }

        if !found_any {
            return Err(DatasetError::NotFound {
                path: path.display().to_string(),
                hint: "Download with: cargo bench --bench download_datasets -- --dataset convomem"
                    .to_string(),
            });
        }

        Ok((all_sessions, all_questions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn eval_common_locomo_fixture_parses_correctly() {
        let loader = LoCoMoLoader;
        let path = Path::new("benches/eval_common/test_data");
        let result = loader.load(path);
        assert!(result.is_ok(), "should parse fixture: {:?}", result.err());
        let (sessions, questions) = result.expect("fixture should parse");
        assert_eq!(sessions.len(), 2, "should have 2 sessions");
        assert_eq!(questions.len(), 1, "should have 1 question");
        assert_eq!(questions[0].question_type, "single-hop");
    }

    #[test]
    fn eval_common_locomo_missing_file_returns_not_found() {
        let loader = LoCoMoLoader;
        let result = loader.load(Path::new("/nonexistent/path"));
        assert!(matches!(result, Err(DatasetError::NotFound { .. })));
    }

    #[test]
    fn eval_common_longmemeval_fixture_parses_correctly() {
        let loader = LongMemEvalLoader;
        let path = Path::new("benches/eval_common/test_data");
        let result = loader.load(path);
        assert!(result.is_ok(), "should parse fixture: {:?}", result.err());
        let (sessions, questions) = result.expect("fixture should parse");
        assert_eq!(sessions.len(), 1);
        assert_eq!(questions.len(), 1);
    }

    #[test]
    fn eval_common_convomem_fixture_parses_correctly() {
        let loader = ConvoMemLoader;
        let path = Path::new("benches/eval_common/test_data/convomem");
        let result = loader.load(path);
        assert!(result.is_ok(), "should parse fixture: {:?}", result.err());
        let (sessions, questions) = result.expect("fixture should parse");
        assert_eq!(sessions.len(), 1);
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].question_type, "test_category");
    }
}
