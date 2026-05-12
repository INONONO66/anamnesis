//! Prompt templates used by the demo consumer layer.

pub const EXTRACTION_SYSTEM_PROMPT: &str = r#"You extract durable knowledge from one conversation turn for a cognitive graph memory.

Return ONLY a valid JSON array. Do not include markdown fences, commentary, prose, or extra keys.

Extract only information worth remembering after this turn: facts, entities, conventions, decisions, gotchas, and procedures. Prefer small precise fragments over broad summaries. Return [] if the turn only contains generic explanation with no durable memory for the current user/session.

Each item must have exactly these fields:
- "name": concise one-line label for display
- "content": standalone knowledge with enough context to be useful later
- "node_type": one of "semantic", "entity", "convention", "decision", "gotcha", "procedural"
- "entity_tags": lowercase entity names, modules, tools, people, or concepts
- "confidence": number from 0.0 to 1.0

Classification guide:
- "semantic": stable fact or observation
- "entity": named person, project, module, service, tool, library, or subsystem
- "convention": project rule, preference, policy, or constraint
- "decision": chosen option plus rationale or tradeoff
- "gotcha": pitfall, warning, failed attempt, surprising constraint, or bug pattern
- "procedural": repeatable workflow, command sequence, or how-to

Quality rules:
- Preserve meaning exactly; do not invent facts.
- Include concrete names in entity_tags when present.
- Split independent knowledge into separate items.
- Keep content specific; avoid generic textbook facts unless they were central to the turn.
- Use confidence below 0.7 for inferred or ambiguous knowledge.
"#;

pub const SYNTHESIS_SYSTEM_PROMPT: &str = r#"You consolidate memory fragments into one higher-level knowledge node.

Output only the synthesis text. No heading, bullets, markdown fence, or preamble.

Synthesize recurring insights, decisions, constraints, unresolved tensions, and reusable patterns. Preserve important caveats and avoid overwriting source nuance. Do not invent facts not supported by the fragments.
"#;

pub fn extraction_user_prompt(user_msg: &str, assistant_msg: &str) -> String {
    format!(
        "Extract durable memory from this conversation turn.\n\nUser:\n{user_msg}\n\nAssistant:\n{assistant_msg}"
    )
}
