#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp(u64);

impl Timestamp {
    pub const fn from_millis(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_millis(self) -> u64 {
        self.0
    }
}

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TemporalValidity {
    valid_from: Option<Timestamp>,
    valid_until: Option<Timestamp>,
}

impl TemporalValidity {
    pub const ALWAYS: Self = Self {
        valid_from: None,
        valid_until: None,
    };

    pub const fn new(
        valid_from: Option<Timestamp>,
        valid_until: Option<Timestamp>,
    ) -> Result<Self, TemporalError> {
        if let (Some(start), Some(end)) = (valid_from, valid_until)
            && start.as_millis() >= end.as_millis()
        {
            return Err(TemporalError::InvertedRange);
        }
        Ok(Self {
            valid_from,
            valid_until,
        })
    }

    pub const fn valid_from(self) -> Option<Timestamp> {
        self.valid_from
    }

    pub const fn valid_until(self) -> Option<Timestamp> {
        self.valid_until
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemporalError {
    InvertedRange,
}

pub const fn valid_at(validity: TemporalValidity, timestamp: Timestamp) -> bool {
    if let Some(start) = validity.valid_from()
        && timestamp.as_millis() < start.as_millis()
    {
        return false;
    }
    if let Some(end) = validity.valid_until()
        && timestamp.as_millis() >= end.as_millis()
    {
        return false;
    }
    true
}
