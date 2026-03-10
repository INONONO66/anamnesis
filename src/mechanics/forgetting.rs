//! Forgetting mechanics: temporal decay and reinforcement

/// Forgetting mechanics for salience decay
pub struct Forgetting;

impl Forgetting {
    /// Exponential decay of salience over time
    /// salience_new = salience_old * exp(-decay_rate * time_elapsed)
    pub fn exponential_decay(salience: f64, decay_rate: f64, time_elapsed: u64) -> f64 {
        let exponent = -decay_rate * time_elapsed as f64;
        salience * exponent.exp()
    }

    /// Polynomial decay of salience over time
    /// salience_new = salience_old / (1 + decay_rate * time_elapsed)
    pub fn polynomial_decay(salience: f64, decay_rate: f64, time_elapsed: u64) -> f64 {
        salience / (1.0 + decay_rate * time_elapsed as f64)
    }

    /// Reinforcement on access — strengthen a node when touched
    pub fn reinforce(salience: f64, reinforcement_amount: f64) -> f64 {
        (salience + reinforcement_amount).min(1.0)
    }

    /// Determine if a node should be pruned (too low salience)
    pub fn should_prune(salience: f64, prune_threshold: f64) -> bool {
        salience < prune_threshold
    }

    /// Apply decay to all nodes
    pub fn apply_decay(
        saliences: &mut [(u64, f64)],
        last_accessed: &[(u64, u64)],
        current_time: u64,
        decay_rate: f64,
        use_exponential: bool,
    ) {
        let access_map: std::collections::HashMap<u64, u64> =
            last_accessed.iter().copied().collect();

        for (node_id, salience) in saliences.iter_mut() {
            let last_access = access_map.get(node_id).copied().unwrap_or(current_time);
            let time_elapsed = current_time.saturating_sub(last_access);

            *salience = if use_exponential {
                Self::exponential_decay(*salience, decay_rate, time_elapsed)
            } else {
                Self::polynomial_decay(*salience, decay_rate, time_elapsed)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exponential_decay() {
        let salience = 1.0;
        let decayed = Forgetting::exponential_decay(salience, 0.1, 10);
        assert!(decayed < salience);
        assert!(decayed > 0.0);
    }

    #[test]
    fn test_polynomial_decay() {
        let salience = 1.0;
        let decayed = Forgetting::polynomial_decay(salience, 0.1, 10);
        assert!(decayed < salience);
        assert!(decayed > 0.0);
    }

    #[test]
    fn test_reinforce() {
        let salience = 0.5;
        let reinforced = Forgetting::reinforce(salience, 0.3);
        assert!(reinforced > salience);
        assert!(reinforced <= 1.0);
    }

    #[test]
    fn test_should_prune() {
        assert!(Forgetting::should_prune(0.01, 0.1));
        assert!(!Forgetting::should_prune(0.5, 0.1));
    }
}
