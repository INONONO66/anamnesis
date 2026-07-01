#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopePath(String);

impl ScopePath {
    pub fn universal() -> Self {
        Self("universal".to_owned())
    }

    pub fn from_segments<const N: usize>(segments: [&str; N]) -> Result<Self, ScopeError> {
        if N == 0 {
            return Err(ScopeError::Empty);
        }
        let mut path = String::new();
        for segment in segments {
            validate_segment(segment)?;
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(segment);
        }
        Ok(Self(path))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn contains(&self, other: &Self) -> bool {
        if self.0 == "universal" {
            return true;
        }
        if self.0 == other.0 {
            return true;
        }
        other
            .0
            .strip_prefix(self.0.as_str())
            .is_some_and(|suffix| suffix.starts_with('/'))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeError {
    Empty,
    EmptySegment,
    ReservedUniversalSegment,
    ContainsSlash,
}

fn validate_segment(segment: &str) -> Result<(), ScopeError> {
    if segment.is_empty() {
        return Err(ScopeError::EmptySegment);
    }
    if segment == "universal" {
        return Err(ScopeError::ReservedUniversalSegment);
    }
    if segment.contains('/') {
        return Err(ScopeError::ContainsSlash);
    }
    Ok(())
}
