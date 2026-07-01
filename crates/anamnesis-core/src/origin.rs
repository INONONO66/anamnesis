use crate::ScopePath;

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceKind {
    AgentObservation,
    HumanInput,
    DocumentExtract,
    SystemEvent,
    Inferred,
    External,
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Confidence(f64);

impl Confidence {
    pub const MIN: f64 = 0.0;
    pub const MAX: f64 = 1.0;

    pub fn new(value: f64) -> Result<Self, OriginError> {
        if !value.is_finite() {
            return Err(OriginError::InvalidConfidence);
        }
        if !(Self::MIN..=Self::MAX).contains(&value) {
            return Err(OriginError::InvalidConfidence);
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Debug, Clone, PartialEq)]
pub struct Origin {
    agent_id: String,
    source_kind: SourceKind,
    session_id: String,
    scope: ScopePath,
    confidence: Confidence,
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Debug, Clone, PartialEq)]
pub struct OriginInput {
    pub agent_id: String,
    pub source_kind: SourceKind,
    pub session_id: String,
    pub scope: ScopePath,
    pub confidence: f64,
}

impl Origin {
    pub fn new(input: OriginInput) -> Result<Self, OriginError> {
        let agent_id = non_empty(input.agent_id, OriginError::EmptyAgentId)?;
        let session_id = non_empty(input.session_id, OriginError::EmptySessionId)?;
        Ok(Self {
            agent_id,
            source_kind: input.source_kind,
            session_id,
            scope: input.scope,
            confidence: Confidence::new(input.confidence)?,
        })
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub const fn source_kind(&self) -> SourceKind {
        self.source_kind
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub const fn scope(&self) -> &ScopePath {
        &self.scope
    }

    pub const fn confidence(&self) -> Confidence {
        self.confidence
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginError {
    EmptyAgentId,
    EmptySessionId,
    InvalidConfidence,
}

fn non_empty(value: String, error: OriginError) -> Result<String, OriginError> {
    if value.is_empty() {
        return Err(error);
    }
    Ok(value)
}
