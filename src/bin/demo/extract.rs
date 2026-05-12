//! LLM-based knowledge extraction for demo conversation turns.

use anamnesis::graph::KnowledgeType;
use serde::{Deserialize, Serialize};

use super::ollama::{ChatMessage, OllamaClient};

const EXTRACTION_SYSTEM_PROMPT: &str = r#"You extract durable knowledge from a single conversation turn for a cognitive graph.

Return ONLY a valid JSON array. Do not include markdown fences, commentary, or extra keys.

Extract facts, entities, decisions, conventions, gotchas, and procedural knowledge that should be remembered beyond this turn. Prefer precise fragments over broad summaries. If there is nothing durable to remember, return [].

Each array item must have exactly these fields:
- "name": short one-line label (L0), concise and scannable
- "content": full extracted knowledge with enough context to stand alone
- "node_type": one of "semantic", "entity", "convention", "decision", "gotcha", "procedural"
- "entity_tags": related entity names as lowercase strings where possible
- "confidence": number from 0.0 to 1.0

Classification guide:
- "semantic": factual knowledge or stable observation
- "entity": named concept, module, person, service, library, or subsystem
- "convention": project rule, norm, preference, or constraint
- "decision": choice made with rationale or tradeoff
- "gotcha": pitfall, warning, bug pattern, or surprising constraint
- "procedural": how-to, workflow, command sequence, or repeatable method

Quality rules:
- Preserve the original meaning; do not invent facts.
- Include concrete names in entity_tags when present.
- Keep name short; put details in content.
- Split independent pieces of knowledge into separate items.
- Use confidence below 0.7 for inferred or ambiguous knowledge.
"#;

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
    client: &OllamaClient,
    user_msg: &str,
    assistant_msg: &str,
) -> Result<ExtractionResult, String> {
    let prompt = format!(
        "Extract durable knowledge from this conversation turn.\n\nUser:\n{user_msg}\n\nAssistant:\n{assistant_msg}"
    );
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
