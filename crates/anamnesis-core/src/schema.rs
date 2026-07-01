#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NodeKind {
    Source,
    Fragment,
    Entity,
    Memory,
}

impl NodeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Fragment => "fragment",
            Self::Entity => "entity",
            Self::Memory => "memory",
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MemoryKind {
    Fact,
    State,
    Preference,
    Event,
    Procedure,
    Decision,
    Problem,
    Hypothesis,
    Evidence,
    Resolution,
    Guardrail,
    Summary,
}

impl MemoryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::State => "state",
            Self::Preference => "preference",
            Self::Event => "event",
            Self::Procedure => "procedure",
            Self::Decision => "decision",
            Self::Problem => "problem",
            Self::Hypothesis => "hypothesis",
            Self::Evidence => "evidence",
            Self::Resolution => "resolution",
            Self::Guardrail => "guardrail",
            Self::Summary => "summary",
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntityKind {
    Person,
    Agent,
    Organization,
    Project,
    File,
    Tool,
    Concept,
    Place,
    Other,
}

impl EntityKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Agent => "agent",
            Self::Organization => "organization",
            Self::Project => "project",
            Self::File => "file",
            Self::Tool => "tool",
            Self::Concept => "concept",
            Self::Place => "place",
            Self::Other => "other",
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EdgeKind {
    Contains,
    Sequence,
    References,
    Derives,
    Associates,
    Contrasts,
    Resolves,
}

impl EdgeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Sequence => "sequence",
            Self::References => "references",
            Self::Derives => "derives",
            Self::Associates => "associates",
            Self::Contrasts => "contrasts",
            Self::Resolves => "resolves",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_kinds_have_stable_keys() {
        assert_eq!(NodeKind::Memory.as_str(), "memory");
        assert_eq!(MemoryKind::Decision.as_str(), "decision");
        assert_eq!(EntityKind::Agent.as_str(), "agent");
        assert_eq!(EdgeKind::Sequence.as_str(), "sequence");
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serde_uses_stable_schema_keys() {
        assert_eq!(
            serde_json::to_string(&NodeKind::Memory).unwrap(),
            "\"memory\""
        );
        assert_eq!(
            serde_json::to_string(&MemoryKind::Decision).unwrap(),
            "\"decision\""
        );
        assert_eq!(
            serde_json::to_string(&EntityKind::Agent).unwrap(),
            "\"agent\""
        );
        assert_eq!(
            serde_json::to_string(&EdgeKind::Sequence).unwrap(),
            "\"sequence\""
        );
    }
}
