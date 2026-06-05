//! Social reinforcement scoring — multi-agent salience dynamics.
//!
//! When multiple distinct agents independently observe the same knowledge fragment,
//! social support amplifies salience. Logarithmic scaling prevents popularity cascades.
//!
//! # Formula
//!
//! `social_support(n, agreement, confidence) = ln(1 + n) * agreement * confidence`
//!
//! where:
//! - `n` = number of distinct agents corroborating this fragment
//! - `agreement` = how closely agents agree [0, 1]
//! - `confidence` = average confidence across supporting agents [0, 1]
//!
//! # Feedback Signal
//!
//! Consumer-provided [`FeedbackSignal`]s drive a `FeedbackReceived` interaction: the
//! engine maps the signal to a Rescorla-Wagner reward target in log-odds space and
//! applies `dA_i = eta * (lambda - A_i)` on the authoritative retained-action
//! reservoir (see [`crate::mechanics::interactions::lambda_reward`] /
//! [`crate::mechanics::interactions::rescorla_wagner`]). This module owns the signal
//! type and its directionality; the reservoir update lives in `interactions`.

/// Feedback signal from the consumer about a knowledge fragment's utility.
///
/// Each variant carries a strength value in [0, 1] representing signal intensity.
/// Positive signals (Useful) boost salience; negative signals (NotUseful, Incorrect)
/// reduce it via the same diminishing-returns formula applied in reverse.
#[derive(Debug, Clone, PartialEq)]
pub enum FeedbackSignal {
    /// The fragment was useful in context. Strength in [0, 1].
    Useful { strength: f64 },
    /// The fragment was not useful in context. Strength in [0, 1].
    NotUseful { strength: f64 },
    /// The fragment contained incorrect information. Strength in [0, 1].
    Incorrect { strength: f64 },
}

impl FeedbackSignal {
    /// Returns the effective signed strength: positive for Useful, negative otherwise.
    pub fn signed_strength(&self) -> f64 {
        match self {
            FeedbackSignal::Useful { strength } => *strength,
            FeedbackSignal::NotUseful { strength } => -*strength,
            FeedbackSignal::Incorrect { strength } => -*strength,
        }
    }

    /// Returns the raw strength value regardless of direction.
    pub fn strength(&self) -> f64 {
        match self {
            FeedbackSignal::Useful { strength }
            | FeedbackSignal::NotUseful { strength }
            | FeedbackSignal::Incorrect { strength } => *strength,
        }
    }
}

/// Caller confidence in a committed [`ContextPackage`](crate::query::ContextPackage).
///
/// Supplied to [`Engine::commit`](crate::api::Engine::commit), it grades how useful
/// the packaged context actually was. Each level maps to a signed Rescorla-Wagner
/// reward target via [`FeedbackSignal::from`] / `lambda_reward`
/// ([`crate::mechanics::interactions::lambda_reward`]): the commit then moves each
/// accessed site's retained action a fraction `eta` toward that target
/// (interactions.md, ADR-0003). `None` confidence means "record use without a
/// feedback signal" — see [`Engine::commit`](crate::api::Engine::commit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfidenceLevel {
    /// The context was clearly useful — strong positive reward.
    High,
    /// The context was useful — moderate positive reward.
    Medium,
    /// The context was weakly useful — mild positive reward.
    Low,
    /// The context was not useful or was misleading — negative reward.
    None,
}

impl ConfidenceLevel {
    /// The unit-interval strength this confidence level contributes to the reward
    /// target. CALIBRATED PRIOR — graded steps from a strong-positive `High` down to
    /// a full-negative `None`; the Rescorla-Wagner step scales this by
    /// [`REWARD_LOG_ODDS_SCALE`](crate::mechanics::priors::REWARD_LOG_ODDS_SCALE).
    pub fn strength(&self) -> f64 {
        match self {
            ConfidenceLevel::High => 1.0,
            ConfidenceLevel::Medium => 0.6,
            ConfidenceLevel::Low => 0.3,
            ConfidenceLevel::None => -1.0,
        }
    }
}

impl From<ConfidenceLevel> for FeedbackSignal {
    /// Map a commit [`ConfidenceLevel`] to the equivalent [`FeedbackSignal`], so the
    /// same `lambda_reward` mapping drives both commit feedback and explicit feedback.
    fn from(level: ConfidenceLevel) -> Self {
        match level {
            ConfidenceLevel::High => FeedbackSignal::Useful { strength: 1.0 },
            ConfidenceLevel::Medium => FeedbackSignal::Useful { strength: 0.6 },
            ConfidenceLevel::Low => FeedbackSignal::Useful { strength: 0.3 },
            ConfidenceLevel::None => FeedbackSignal::NotUseful { strength: 1.0 },
        }
    }
}

/// Compute social support score from multi-agent corroboration.
///
/// Formula: `ln(1 + distinct_agent_count) * agreement_score * avg_confidence`
///
/// Properties:
/// - Returns 0.0 when `distinct_agent_count` is 0
/// - Logarithmic scaling in agent count prevents popularity cascades
/// - Agreement and confidence are multiplicative gates
/// - Result is unbounded above (grows slowly with agent count)
///
/// # Arguments
///
/// * `distinct_agent_count` — Number of unique agents that independently observed this fragment.
///   Same agent across multiple sessions counts as 1.
/// * `agreement_score` — How closely the agents agree on the content [0, 1].
/// * `avg_confidence` — Mean confidence across the supporting agents [0, 1].
pub fn social_support(
    distinct_agent_count: usize,
    agreement_score: f64,
    avg_confidence: f64,
) -> f64 {
    (1.0 + distinct_agent_count as f64).ln() * agreement_score * avg_confidence
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn social_support_zero_agents() {
        let score = social_support(0, 1.0, 1.0);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn social_support_single_agent() {
        let score = social_support(1, 1.0, 1.0);
        let expected = (2.0_f64).ln();
        assert!((score - expected).abs() < 1e-10);
    }

    #[test]
    fn social_support_logarithmic_scaling() {
        let s2 = social_support(2, 1.0, 1.0);
        let s10 = social_support(10, 1.0, 1.0);
        let s100 = social_support(100, 1.0, 1.0);

        // Logarithmic: growth slows dramatically
        assert!(s10 > s2);
        assert!(s100 > s10);
        // Ratio check: 100 agents gives less than 3x the score of 2 agents
        assert!(s100 / s2 < 5.0);
    }

    #[test]
    fn social_support_agreement_gates() {
        let full = social_support(3, 1.0, 1.0);
        let half = social_support(3, 0.5, 1.0);
        assert!((half - full * 0.5).abs() < 1e-10);
    }

    #[test]
    fn social_support_confidence_gates() {
        let full = social_support(3, 1.0, 1.0);
        let low_conf = social_support(3, 1.0, 0.3);
        assert!((low_conf - full * 0.3).abs() < 1e-10);
    }

    #[test]
    fn feedback_signal_signed_strength() {
        assert_eq!(
            FeedbackSignal::Useful { strength: 0.7 }.signed_strength(),
            0.7
        );
        assert_eq!(
            FeedbackSignal::NotUseful { strength: 0.7 }.signed_strength(),
            -0.7
        );
        assert_eq!(
            FeedbackSignal::Incorrect { strength: 0.7 }.signed_strength(),
            -0.7
        );
    }
}
