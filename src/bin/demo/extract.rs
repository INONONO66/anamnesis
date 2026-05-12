//! LLM-based knowledge extraction for demo conversation turns.

use anamnesis::graph::KnowledgeType;
use serde::{Deserialize, Serialize};

use super::llm::{ChatMessage, LocalLlmClient};
use super::prompts::{EXTRACTION_SYSTEM_PROMPT, extraction_user_prompt};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFact {
    pub name: String,
    pub content: String,
    pub node_type: String,
    pub entity_tags: Vec<String>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub facts: Vec<ExtractedFact>,
}

pub async fn extract_knowledge(
    client: &LocalLlmClient,
    user_msg: &str,
    assistant_msg: &str,
) -> Result<ExtractionResult, String> {
    let prompt = extraction_user_prompt(user_msg, assistant_msg);
    let messages = [ChatMessage {
        role: "user".to_string(),
        content: prompt,
    }];

    let response = client
        .chat_with_system(EXTRACTION_SYSTEM_PROMPT, &messages)
        .await?;

    Ok(parse_extraction_response(&response).unwrap_or_default())
}

pub fn map_node_type(type_str: &str) -> KnowledgeType {
    match type_str.trim().to_ascii_lowercase().as_str() {
        "semantic" => KnowledgeType::Semantic,
        "entity" => KnowledgeType::Entity,
        "convention" => KnowledgeType::Convention,
        "decision" => KnowledgeType::Decision,
        "gotcha" => KnowledgeType::Gotcha,
        "procedural" => KnowledgeType::Procedural,
        _ => KnowledgeType::Semantic,
    }
}

fn parse_extraction_response(response: &str) -> Option<ExtractionResult> {
    let json = extract_json_array(response.trim());
    serde_json::from_str::<Vec<ExtractedFact>>(json)
        .ok()
        .map(|facts| ExtractionResult { facts })
}

fn extract_json_array(response: &str) -> &str {
    let trimmed = response.trim();
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);

    match (unfenced.find('['), unfenced.rfind(']')) {
        (Some(start), Some(end)) if start <= end => &unfenced[start..=end],
        _ => unfenced,
    }
}
