//! Deterministic time-cue extraction from query text.
//!
//! Query-local only (potential-landscape.md): parsed cues feed the
//! `beta_temporal * temporal_score_i` potential term and are never stored.
//! No LLM, no locale data — explicit dates only.

/// An inclusive UTC time range parsed from the query text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeRange {
    pub start: u64,
    pub end: u64,
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const DAY_SECS: u64 = 86_400;

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = ((month + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn day_epoch(year: i64, month: u32, day: u32) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !(1970..=2200).contains(&year) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    (days >= 0).then(|| days as u64 * DAY_SECS)
}

fn month_range(year: i64, month: u32) -> Option<TimeRange> {
    let start = day_epoch(year, month, 1)?;
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let end = day_epoch(next_year, next_month, 1)? - 1;
    Some(TimeRange { start, end })
}

/// Extract explicit date cues: `YYYY-MM-DD`, `YYYY/MM/DD`,
/// `D Month YYYY`, `Month D YYYY` (comma tolerated), `Month YYYY`.
pub(crate) fn parse_time_cues(text: &str) -> Vec<TimeRange> {
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || c == ',' || c == '?' || c == '.' || c == '!')
        .filter(|t| !t.is_empty())
        .collect();
    let mut ranges: Vec<TimeRange> = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        // ISO-like: YYYY-MM-DD or YYYY/MM/DD.
        for separator in ['-', '/'] {
            if token.matches(separator).count() == 2 {
                let mut parts = token.split(separator);
                if let (Some(Ok(y)), Some(Ok(m)), Some(Ok(d))) = (
                    parts.next().map(str::parse::<i64>),
                    parts.next().map(str::parse::<u32>),
                    parts.next().map(str::parse::<u32>),
                ) {
                    if let Some(start) = day_epoch(y, m, d) {
                        ranges.push(TimeRange {
                            start,
                            end: start + DAY_SECS - 1,
                        });
                    }
                }
            }
        }

        // Month-name forms.
        if let Some(position) = MONTHS.iter().position(|m| m == token) {
            let month = position as u32 + 1;
            let day = [index.wrapping_sub(1), index + 1]
                .iter()
                .filter_map(|&n| tokens.get(n))
                .filter_map(|t| t.parse::<u32>().ok())
                .find(|d| (1..=31).contains(d));
            let year = tokens[index.saturating_sub(2)..(index + 3).min(tokens.len())]
                .iter()
                .filter_map(|t| t.parse::<i64>().ok())
                .find(|y| (1970..=2200).contains(y));
            if let Some(year) = year {
                match day.and_then(|d| day_epoch(year, month, d)) {
                    Some(start) => ranges.push(TimeRange {
                        start,
                        end: start + DAY_SECS - 1,
                    }),
                    None => {
                        if let Some(range) = month_range(year, month) {
                            ranges.push(range);
                        }
                    }
                }
            }
        }
    }
    ranges.dedup();
    ranges
}

/// Proximity of a site timestamp to the nearest cue range: `1.0` inside a
/// range, decaying as `exp(-days_outside / TEMPORAL_PROXIMITY_DECAY_DAYS)`.
/// Sites with no meaningful timestamp (0) and empty cue sets are inert.
pub(crate) fn temporal_proximity(timestamp: u64, cues: &[TimeRange]) -> f64 {
    if cues.is_empty() || timestamp == 0 {
        return 0.0;
    }
    cues.iter()
        .map(|range| {
            if timestamp >= range.start && timestamp <= range.end {
                1.0
            } else {
                let distance = if timestamp < range.start {
                    range.start - timestamp
                } else {
                    timestamp - range.end
                };
                let days = distance as f64 / DAY_SECS as f64;
                (-days / crate::mechanics::priors::TEMPORAL_PROXIMITY_DECAY_DAYS).exp()
            }
        })
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAY_8_2023: u64 = 1_683_504_000; // 2023-05-08 00:00 UTC

    #[test]
    fn parses_natural_language_date() {
        let cues = parse_time_cues("what happened on 8 May 2023");
        assert_eq!(cues.len(), 1, "expected one cue, got {cues:?}");
        assert_eq!(
            cues[0].start, MAY_8_2023,
            "start must be 2023-05-08 00:00 UTC"
        );
        assert_eq!(
            cues[0].end,
            MAY_8_2023 + 86_400 - 1,
            "end must be end of day"
        );
    }

    #[test]
    fn parses_iso_date() {
        let cues = parse_time_cues("event on 2023-05-08 was notable");
        assert_eq!(cues.len(), 1, "expected one cue");
        assert_eq!(cues[0].start, MAY_8_2023);
    }

    #[test]
    fn parses_month_year_range() {
        let cues = parse_time_cues("events in May 2023");
        assert_eq!(cues.len(), 1, "expected one cue for month range");
        // May 2023: 2023-05-01 00:00 UTC = 1682899200
        assert_eq!(cues[0].start, 1_682_899_200, "start must be 2023-05-01");
        // End must be 2023-05-31 23:59:59 = 1685577599
        assert_eq!(
            cues[0].end, 1_685_577_599,
            "end must be last second of May 2023"
        );
    }

    #[test]
    fn no_dates_returns_empty() {
        let cues = parse_time_cues("beach trip planning notes");
        assert!(cues.is_empty(), "expected no cues, got {cues:?}");
    }

    #[test]
    fn bare_number_produces_nothing() {
        let cues = parse_time_cues("8 items in list");
        assert!(
            cues.is_empty(),
            "bare number without context must not produce cue"
        );
    }

    #[test]
    fn year_alone_produces_nothing() {
        let cues = parse_time_cues("something in 2023");
        assert!(
            cues.is_empty(),
            "year alone without month must not produce cue"
        );
    }

    #[test]
    fn proximity_one_inside_range() {
        let range = TimeRange {
            start: MAY_8_2023,
            end: MAY_8_2023 + 86_400 - 1,
        };
        let score = temporal_proximity(MAY_8_2023 + 3600, &[range]);
        assert_eq!(score, 1.0, "inside range must score 1.0");
    }

    #[test]
    fn proximity_decays_outside_range() {
        let range = TimeRange {
            start: MAY_8_2023,
            end: MAY_8_2023 + 86_400 - 1,
        };
        // 7 days before — should decay by exp(-7/7) = exp(-1)
        let ts = MAY_8_2023 - 7 * 86_400;
        let score = temporal_proximity(ts, &[range]);
        let expected = (-7.0 / crate::mechanics::priors::TEMPORAL_PROXIMITY_DECAY_DAYS).exp();
        assert!(
            (score - expected).abs() < 1e-12,
            "score {score} must match decay formula {expected}"
        );
    }

    #[test]
    fn proximity_zero_for_zero_timestamp() {
        let range = TimeRange {
            start: MAY_8_2023,
            end: MAY_8_2023 + 86_400 - 1,
        };
        assert_eq!(temporal_proximity(0, &[range]), 0.0);
    }

    #[test]
    fn proximity_zero_for_empty_cues() {
        assert_eq!(temporal_proximity(MAY_8_2023, &[]), 0.0);
    }
}
