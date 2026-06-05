//! Bitemporal validity — the single canonical `valid_at` gate.
//!
//! Validity is the **half-open** interval `[valid_from, valid_until)`: the lower
//! bound is inclusive and the upper bound is exclusive. A `None` bound is
//! unbounded on that side. All node and edge validity checks route through this
//! one predicate so the semantics can never diverge across call sites.
//!
//! See `docs/02-knowledge-model/temporal-model.md`.

use crate::graph::Timestamp;

/// Returns whether validity bounds `[valid_from, valid_until)` contain `as_of`.
///
/// - `valid_from` is inclusive: `as_of == valid_from` is valid.
/// - `valid_until` is exclusive: `as_of == valid_until` is **not** valid.
/// - A `None` bound imposes no constraint on that side.
pub fn valid_at(
    valid_from: Option<Timestamp>,
    valid_until: Option<Timestamp>,
    as_of: Timestamp,
) -> bool {
    let from_ok = valid_from.is_none_or(|from| from <= as_of);
    let until_ok = valid_until.is_none_or(|until| as_of < until);
    from_ok && until_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbounded_is_always_valid() {
        assert!(valid_at(None, None, Timestamp(0)));
        assert!(valid_at(None, None, Timestamp(u64::MAX)));
    }

    #[test]
    fn lower_bound_is_inclusive() {
        assert!(valid_at(Some(Timestamp(5)), None, Timestamp(5)));
        assert!(!valid_at(Some(Timestamp(5)), None, Timestamp(4)));
        assert!(valid_at(Some(Timestamp(5)), None, Timestamp(6)));
    }

    #[test]
    fn upper_bound_is_exclusive() {
        assert!(!valid_at(None, Some(Timestamp(10)), Timestamp(10)));
        assert!(valid_at(None, Some(Timestamp(10)), Timestamp(9)));
        assert!(!valid_at(None, Some(Timestamp(10)), Timestamp(11)));
    }

    #[test]
    fn closed_open_interval() {
        // [5, 10): 5 in, 10 out
        assert!(valid_at(Some(Timestamp(5)), Some(Timestamp(10)), Timestamp(5)));
        assert!(valid_at(Some(Timestamp(5)), Some(Timestamp(10)), Timestamp(9)));
        assert!(!valid_at(Some(Timestamp(5)), Some(Timestamp(10)), Timestamp(10)));
        assert!(!valid_at(Some(Timestamp(5)), Some(Timestamp(10)), Timestamp(4)));
    }
}
