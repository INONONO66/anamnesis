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
//! Consumer-provided feedback adjusts salience with diminishing returns:
//! `s += η * signal_strength * (1 - s)` where η = social_learning_rate.

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

/// Apply feedback signal to current salience using diminishing returns.
///
/// Formula: `s + η * signal_strength * (1 - s)` for positive signals,
/// `s - η * |signal_strength| * s` for negative signals.
///
/// Positive signals approach 1.0 asymptotically (diminishing returns).
/// Negative signals approach 0.0 asymptotically (diminishing reduction).
/// Result is always clamped to [0, 1].
///
/// # Arguments
///
/// * `current_salience` — Current salience value [0, 1].
/// * `signal` — The feedback signal with direction and strength.
/// * `learning_rate` — η, the social learning rate (typically 0.15).
pub fn apply_feedback_to_salience(
    current_salience: f64,
    signal: &FeedbackSignal,
    learning_rate: f64,
) -> f64 {
    let signed = signal.signed_strength();
    let new_salience = if signed >= 0.0 {
        // Positive: s + η * strength * (1 - s), diminishing returns toward 1.0
        current_salience + learning_rate * signed * (1.0 - current_salience)
    } else {
        // Negative: s - η * |strength| * s, diminishing reduction toward 0.0
        current_salience + learning_rate * signed * current_salience
    };
    new_salience.clamp(0.0, 1.0)
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
    fn feedback_useful_increases_salience() {
        let signal = FeedbackSignal::Useful { strength: 1.0 };
        let new_s = apply_feedback_to_salience(0.5, &signal, 0.15);
        assert!(new_s > 0.5);
        // Expected: 0.5 + 0.15 * 1.0 * (1 - 0.5) = 0.575
        assert!((new_s - 0.575).abs() < 1e-10);
    }

    #[test]
    fn feedback_not_useful_decreases_salience() {
        let signal = FeedbackSignal::NotUseful { strength: 1.0 };
        let new_s = apply_feedback_to_salience(0.5, &signal, 0.15);
        assert!(new_s < 0.5);
        // Expected: 0.5 - 0.15 * 1.0 * 0.5 = 0.425
        assert!((new_s - 0.425).abs() < 1e-10);
    }

    #[test]
    fn feedback_incorrect_decreases_salience() {
        let signal = FeedbackSignal::Incorrect { strength: 0.8 };
        let new_s = apply_feedback_to_salience(0.6, &signal, 0.15);
        assert!(new_s < 0.6);
        // Expected: 0.6 - 0.15 * 0.8 * 0.6 = 0.528
        assert!((new_s - 0.528).abs() < 1e-10);
    }

    #[test]
    fn feedback_diminishing_returns_approaches_one() {
        let signal = FeedbackSignal::Useful { strength: 1.0 };
        let mut s = 0.5;
        for _ in 0..100 {
            s = apply_feedback_to_salience(s, &signal, 0.15);
        }
        // After many applications, approaches 1.0 but never exceeds
        assert!(s > 0.99);
        assert!(s <= 1.0);
    }

    #[test]
    fn feedback_diminishing_reduction_approaches_zero() {
        let signal = FeedbackSignal::NotUseful { strength: 1.0 };
        let mut s = 0.5;
        for _ in 0..100 {
            s = apply_feedback_to_salience(s, &signal, 0.15);
        }
        // After many applications, approaches 0.0 but never goes below
        assert!(s < 0.01);
        assert!(s >= 0.0);
    }

    #[test]
    fn feedback_clamped_to_bounds() {
        let signal = FeedbackSignal::Useful { strength: 1.0 };
        let new_s = apply_feedback_to_salience(1.0, &signal, 0.15);
        assert_eq!(new_s, 1.0);

        let signal = FeedbackSignal::NotUseful { strength: 1.0 };
        let new_s = apply_feedback_to_salience(0.0, &signal, 0.15);
        assert_eq!(new_s, 0.0);
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
