//! Deterministic time-cue extraction from query text.
//!
//! Query-local only (potential-landscape.md): parsed cues feed the
//! `beta_temporal * temporal_score_i` potential term and are never stored.
//! No LLM, no locale data.
//!
//! ## Explicit cues
//! ISO dates (`YYYY-MM-DD`, `YYYY/MM/DD`), standalone calendar years,
//! `D Month YYYY`, `Month D YYYY`, `Month YYYY`, and ordinal calendar
//! weekends such as `the first weekend of August 2023`.
//!
//! ## Relative cues
//! Resolved against `now` (Unix epoch milliseconds). When `now == 0`, relative
//! cues are skipped and explicit cues are still parsed.
//!
//! Patterns: "yesterday", "last week", "last month", "last year",
//! "N days/weeks/months ago" (N 1–99), "a week/month ago",
//! "last summer/winter/spring/fall/autumn".
//!
//! Season convention (northern hemisphere):
//! - spring = Mar 1 – May 31
//! - summer = Jun 1 – Aug 31
//! - fall/autumn = Sep 1 – Nov 30
//! - winter = Dec 1 of year Y – last day of Feb year Y+1
//!
//! "last \<season\>" resolves to the most recent season whose END is strictly
//! before the current day.

/// An inclusive UTC time range parsed from the query text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: u64,
    pub end: u64,
}

/// Granularity retained when a relative expression is resolved against one
/// evidence observation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelativeTimePrecision {
    Day,
    Month,
    Range,
}

/// One reference-blind relative-time resolution from evidence text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelativeTimeResolution {
    pub phrase: String,
    pub range: TimeRange,
    pub precision: RelativeTimePrecision,
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
const DAY_MS: u64 = 86_400_000;

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = ((month + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil`: convert a day count (days since 1970-01-01,
/// may be negative) to a proleptic Gregorian (year, month, day).
/// Howard Hinnant's algorithm — <http://howardhinnant.github.io/date_algorithms.html>
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

fn day_epoch(year: i64, month: u32, day: u32) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || !(1970..=2200).contains(&year) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    (days >= 0).then(|| days as u64 * DAY_MS)
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

fn day_range(day: i64) -> Option<TimeRange> {
    if day < 0 {
        return None;
    }
    let start = day as u64 * DAY_MS;
    Some(TimeRange {
        start,
        end: start + DAY_MS - 1,
    })
}

fn calendar_day_range(year: i64, month: u32, day: u32) -> Option<TimeRange> {
    let epoch = day_epoch(year, month, day)?;
    let day_index = (epoch / DAY_MS) as i64;
    (civil_from_days(day_index) == (year, month, day)).then(|| TimeRange {
        start: epoch,
        end: epoch + DAY_MS - 1,
    })
}

fn calendar_weekend_range(year: i64, month: u32, ordinal: usize) -> Option<TimeRange> {
    if !(1..=5).contains(&ordinal) {
        return None;
    }
    let first_day = days_from_civil(year, month, 1);
    // 1970-01-01 was Thursday. Monday=0, ..., Saturday=5.
    let first_weekday = (first_day + 3).rem_euclid(7);
    let first_saturday = 1 + (5 - first_weekday).rem_euclid(7) as u32;
    let saturday = first_saturday.saturating_add((ordinal.saturating_sub(1) * 7) as u32);
    let saturday_range = calendar_day_range(year, month, saturday)?;
    Some(TimeRange {
        start: saturday_range.start,
        end: saturday_range
            .start
            .saturating_add(DAY_MS.saturating_mul(2))
            .saturating_sub(1),
    })
}

fn weekend_ordinal(token: &str) -> Option<usize> {
    Some(match token {
        "first" | "1st" => 1,
        "second" | "2nd" => 2,
        "third" | "3rd" => 3,
        "fourth" | "4th" => 4,
        "fifth" | "5th" => 5,
        _ => return None,
    })
}

fn ordinal_day(token: &str) -> Option<u32> {
    ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|suffix| token.strip_suffix(suffix))
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|day| (1..=31).contains(day))
}

fn bounded_day_range(start_day: i64, end_day: i64) -> Option<TimeRange> {
    if start_day < 0 || end_day < start_day {
        return None;
    }
    Some(TimeRange {
        start: start_day as u64 * DAY_MS,
        end: end_day as u64 * DAY_MS + DAY_MS - 1,
    })
}

fn relative_count(token: &str) -> Option<i64> {
    token
        .parse::<i64>()
        .ok()
        .filter(|value| (1..=99).contains(value))
        .or_else(|| {
            Some(match token {
                "a" | "an" | "one" => 1,
                "two" => 2,
                "three" => 3,
                "four" => 4,
                "five" => 5,
                "six" => 6,
                "seven" => 7,
                "eight" => 8,
                "nine" => 9,
                "ten" => 10,
                "eleven" => 11,
                "twelve" => 12,
                _ => return None,
            })
        })
}

fn previous_weekday(anchor_day: i64, target_monday_zero: i64) -> i64 {
    let anchor_weekday = (anchor_day + 3).rem_euclid(7);
    let mut distance = (anchor_weekday - target_monday_zero).rem_euclid(7);
    if distance == 0 {
        distance = 7;
    }
    anchor_day - distance
}

fn next_weekday(anchor_day: i64, target_monday_zero: i64) -> i64 {
    let anchor_weekday = (anchor_day + 3).rem_euclid(7);
    let mut distance = (target_monday_zero - anchor_weekday).rem_euclid(7);
    if distance == 0 {
        distance = 7;
    }
    anchor_day + distance
}

fn weekday_index(token: &str) -> Option<i64> {
    Some(match token {
        "monday" | "mon" => 0,
        "tuesday" | "tue" | "tues" => 1,
        "wednesday" | "wed" => 2,
        "thursday" | "thu" | "thur" | "thurs" => 3,
        "friday" | "fri" => 4,
        "saturday" | "sat" => 5,
        "sunday" | "sun" => 6,
        _ => return None,
    })
}

fn push_relative_resolution(
    resolutions: &mut Vec<RelativeTimeResolution>,
    phrase: impl Into<String>,
    range: Option<TimeRange>,
    precision: RelativeTimePrecision,
) {
    let Some(range) = range else {
        return;
    };
    let resolution = RelativeTimeResolution {
        phrase: phrase.into(),
        range,
        precision,
    };
    if !resolutions.iter().any(|existing| existing == &resolution) {
        resolutions.push(resolution);
    }
}

/// Resolve relative expressions in one evidence fragment against its immutable
/// observation time.
///
/// This is deliberately narrower than natural-language temporal reasoning: it
/// handles explicit, auditable calendar expressions and never consults the
/// query answer or any model. The original evidence remains authoritative.
pub(crate) fn resolve_relative_time_cues(
    text: &str,
    observed_at: u64,
) -> Vec<RelativeTimeResolution> {
    if observed_at == 0 {
        return Vec::new();
    }
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return Vec::new();
    }

    let anchor_day = (observed_at / DAY_MS) as i64;
    let (anchor_year, anchor_month, _) = civil_from_days(anchor_day);
    let mut resolutions = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        if let Some(day) = ordinal_day(token) {
            let phrase = if index >= 1 && tokens[index - 1] == "the" {
                format!("the {token}")
            } else {
                (*token).to_owned()
            };
            push_relative_resolution(
                &mut resolutions,
                phrase,
                calendar_day_range(anchor_year, anchor_month, day),
                RelativeTimePrecision::Day,
            );
        }
        match *token {
            "today" | "tonight" => push_relative_resolution(
                &mut resolutions,
                *token,
                day_range(anchor_day),
                RelativeTimePrecision::Day,
            ),
            "yesterday" => push_relative_resolution(
                &mut resolutions,
                "yesterday",
                day_range(anchor_day - 1),
                RelativeTimePrecision::Day,
            ),
            "tomorrow" => push_relative_resolution(
                &mut resolutions,
                "tomorrow",
                day_range(anchor_day + 1),
                RelativeTimePrecision::Day,
            ),
            "last" if index + 1 < tokens.len() => match tokens[index + 1] {
                "night" => push_relative_resolution(
                    &mut resolutions,
                    "last night",
                    day_range(anchor_day - 1),
                    RelativeTimePrecision::Day,
                ),
                "week" => push_relative_resolution(
                    &mut resolutions,
                    "last week",
                    bounded_day_range(anchor_day - 7, anchor_day - 1),
                    RelativeTimePrecision::Range,
                ),
                "month" => {
                    let (year, month) = if anchor_month == 1 {
                        (anchor_year - 1, 12)
                    } else {
                        (anchor_year, anchor_month - 1)
                    };
                    push_relative_resolution(
                        &mut resolutions,
                        "last month",
                        month_range(year, month),
                        RelativeTimePrecision::Month,
                    );
                }
                "year" => push_relative_resolution(
                    &mut resolutions,
                    "last year",
                    bounded_day_range(
                        days_from_civil(anchor_year - 1, 1, 1),
                        days_from_civil(anchor_year, 1, 1) - 1,
                    ),
                    RelativeTimePrecision::Range,
                ),
                "weekend" => {
                    let saturday = previous_weekday(anchor_day, 5);
                    push_relative_resolution(
                        &mut resolutions,
                        "last weekend",
                        bounded_day_range(saturday, saturday + 1),
                        RelativeTimePrecision::Range,
                    );
                }
                weekday => {
                    if let Some(weekday) = weekday_index(weekday) {
                        push_relative_resolution(
                            &mut resolutions,
                            format!("last {}", tokens[index + 1]),
                            day_range(previous_weekday(anchor_day, weekday)),
                            RelativeTimePrecision::Day,
                        );
                    }
                }
            },
            "this" if index + 1 < tokens.len() => match tokens[index + 1] {
                "week" => {
                    let weekday = (anchor_day + 3).rem_euclid(7);
                    push_relative_resolution(
                        &mut resolutions,
                        "this week",
                        bounded_day_range(anchor_day - weekday, anchor_day + (6 - weekday)),
                        RelativeTimePrecision::Range,
                    );
                }
                "month" => push_relative_resolution(
                    &mut resolutions,
                    "this month",
                    month_range(anchor_year, anchor_month),
                    RelativeTimePrecision::Month,
                ),
                _ => {}
            },
            "next" if index + 1 < tokens.len() => match tokens[index + 1] {
                "week" => push_relative_resolution(
                    &mut resolutions,
                    "next week",
                    bounded_day_range(anchor_day + 1, anchor_day + 7),
                    RelativeTimePrecision::Range,
                ),
                "month" => {
                    let (year, month) = if anchor_month == 12 {
                        (anchor_year + 1, 1)
                    } else {
                        (anchor_year, anchor_month + 1)
                    };
                    push_relative_resolution(
                        &mut resolutions,
                        "next month",
                        month_range(year, month),
                        RelativeTimePrecision::Month,
                    );
                }
                "weekend" => {
                    let saturday = next_weekday(anchor_day, 5);
                    push_relative_resolution(
                        &mut resolutions,
                        "next weekend",
                        bounded_day_range(saturday, saturday + 1),
                        RelativeTimePrecision::Range,
                    );
                }
                weekday => {
                    if let Some(weekday) = weekday_index(weekday) {
                        push_relative_resolution(
                            &mut resolutions,
                            format!("next {}", tokens[index + 1]),
                            day_range(next_weekday(anchor_day, weekday)),
                            RelativeTimePrecision::Day,
                        );
                    }
                }
            },
            "ago" | "earlier" if index >= 2 => {
                let Some(count) = relative_count(tokens[index - 2]) else {
                    continue;
                };
                let unit = tokens[index - 1];
                match unit {
                    "day" | "days" => push_relative_resolution(
                        &mut resolutions,
                        format!("{} {unit} {}", tokens[index - 2], token),
                        day_range(anchor_day - count),
                        RelativeTimePrecision::Day,
                    ),
                    "week" | "weeks" => push_relative_resolution(
                        &mut resolutions,
                        format!("{} {unit} {}", tokens[index - 2], token),
                        bounded_day_range(anchor_day - count * 7 - 3, anchor_day - count * 7 + 3),
                        RelativeTimePrecision::Range,
                    ),
                    "month" | "months" => {
                        let month_index = anchor_year * 12 + i64::from(anchor_month) - 1 - count;
                        let year = month_index.div_euclid(12);
                        let month = month_index.rem_euclid(12) as u32 + 1;
                        push_relative_resolution(
                            &mut resolutions,
                            format!("{} {unit} {}", tokens[index - 2], token),
                            month_range(year, month),
                            RelativeTimePrecision::Month,
                        );
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    resolutions
}

/// Extract explicit date cues: `YYYY-MM-DD`, `YYYY/MM/DD`, a standalone
/// calendar year, `D Month YYYY`, `Month D YYYY` (comma tolerated), and
/// `Month YYYY`.
/// When `now != 0`, also resolves relative expressions ("yesterday", "last week", etc.).
/// When `now == 0`, relative cues are skipped and only explicit dates are parsed.
pub(crate) fn parse_time_cues(text: &str, now: u64) -> Vec<TimeRange> {
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| {
            c.is_whitespace() || c == ',' || c == '?' || c == '.' || c == '!' || c == '\''
        })
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
                ) && let Some(start) = day_epoch(y, m, d)
                {
                    ranges.push(TimeRange {
                        start,
                        end: start + DAY_MS - 1,
                    });
                }
            }
        }

        // Standalone calendar year. Month-qualified years are handled below
        // at their narrower granularity. Explicit before/after boundaries are
        // not widened to the named year because this parser does not model
        // open-ended intervals.
        let neighbors = [
            index.checked_sub(2),
            index.checked_sub(1),
            Some(index + 1),
            Some(index + 2),
        ]
        .into_iter()
        .flatten()
        .filter_map(|neighbor| tokens.get(neighbor).copied())
        .collect::<Vec<_>>();
        let has_month_neighbor = neighbors.iter().any(|neighbor| MONTHS.contains(neighbor));
        let has_boundary_neighbor = neighbors
            .iter()
            .any(|neighbor| matches!(*neighbor, "before" | "after"));
        if !has_month_neighbor
            && !has_boundary_neighbor
            && let Ok(year) = token.parse::<i64>()
            && (1970..=2200).contains(&year)
            && let (Some(start), Some(next)) = (day_epoch(year, 1, 1), day_epoch(year + 1, 1, 1))
        {
            ranges.push(TimeRange {
                start,
                end: next - 1,
            });
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
                let ordinal_weekend = index
                    .checked_sub(3)
                    .and_then(|ordinal_index| {
                        let relation = tokens.get(index.saturating_sub(1))?;
                        let weekend = tokens.get(index.saturating_sub(2))?;
                        ((*weekend == "weekend") && matches!(*relation, "of" | "in"))
                            .then(|| weekend_ordinal(tokens[ordinal_index]))
                            .flatten()
                    })
                    .and_then(|ordinal| calendar_weekend_range(year, month, ordinal));
                if let Some(range) = ordinal_weekend {
                    ranges.push(range);
                } else {
                    match day.and_then(|d| day_epoch(year, month, d)) {
                        Some(start) => ranges.push(TimeRange {
                            start,
                            end: start + DAY_MS - 1,
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
    }
    // ---- relative cues (only when now != 0) ----
    if now != 0 {
        let now_days = (now / DAY_MS) as i64;
        let (now_year, now_month, _now_day_of_month) = civil_from_days(now_days);

        // Helper: inclusive TimeRange from start/end day counts (days since epoch).
        let day_range = |start_days: i64, end_days: i64| -> Option<TimeRange> {
            if start_days < 0 || end_days < start_days {
                return None;
            }
            Some(TimeRange {
                start: start_days as u64 * DAY_MS,
                end: end_days as u64 * DAY_MS + DAY_MS - 1,
            })
        };

        // Helper: calendar month range given (year, month).
        let cal_month_range = |y: i64, m: u32| -> Option<TimeRange> { month_range(y, m) };

        // Helper: the most recent season (NH convention) whose end is strictly
        // before now_days.
        //   spring = Mar 1 – May 31
        //   summer = Jun 1 – Aug 31
        //   fall   = Sep 1 – Nov 30
        //   winter = Dec 1 of year Y – last day of Feb year Y+1
        // Pass season_start_month > season_end_month to indicate year-spanning (winter).
        let last_season = |season_start_month: u32, season_end_month: u32| -> Option<TimeRange> {
            for candidate_year in [now_year, now_year - 1] {
                // For winter (start=12, end=2): start is Dec of candidate_year,
                // end is Feb of candidate_year+1.
                let (start_year, end_year) = if season_start_month > season_end_month {
                    (candidate_year, candidate_year + 1)
                } else {
                    (candidate_year, candidate_year)
                };

                // Last day of the end month — use end_year for the leap check.
                let season_end_last_day: u32 = if season_end_month == 2 {
                    let is_leap = (end_year % 4 == 0 && end_year % 100 != 0) || end_year % 400 == 0;
                    if is_leap { 29 } else { 28 }
                } else {
                    let next_m = season_end_month + 1;
                    let (next_y, next_m2) = if next_m > 12 {
                        (end_year + 1, 1u32)
                    } else {
                        (end_year, next_m)
                    };
                    // day_epoch(next_y, next_m2, 1) - 1 gives last second of end month;
                    // convert to days then extract day-of-month.
                    let end_epoch = day_epoch(next_y, next_m2, 1)? - 1;
                    let end_days_val = (end_epoch / DAY_MS) as i64;
                    let (_, _, d) = civil_from_days(end_days_val);
                    d
                };

                let season_start_days = days_from_civil(start_year, season_start_month, 1);
                let season_end_days =
                    days_from_civil(end_year, season_end_month, season_end_last_day);

                // Season must end strictly before today.
                if season_end_days < now_days {
                    return day_range(season_start_days, season_end_days);
                }
            }
            None
        };

        let n = tokens.len();
        let mut i = 0;
        while i < n {
            match tokens[i] {
                "yesterday" => {
                    if let Some(r) = day_range(now_days - 1, now_days - 1) {
                        ranges.push(r);
                    }
                    i += 1;
                }
                "last" if i + 1 < n => match tokens[i + 1] {
                    "week" => {
                        // 7 days ending yesterday: [now-7, now-1]
                        if let Some(r) = day_range(now_days - 7, now_days - 1) {
                            ranges.push(r);
                        }
                        i += 2;
                    }
                    "month" => {
                        let (prev_year, prev_month) = if now_month == 1 {
                            (now_year - 1, 12u32)
                        } else {
                            (now_year, now_month - 1)
                        };
                        if let Some(r) = cal_month_range(prev_year, prev_month) {
                            ranges.push(r);
                        }
                        i += 2;
                    }
                    "year" => {
                        let prev_year = now_year - 1;
                        let start = days_from_civil(prev_year, 1, 1);
                        let end = days_from_civil(now_year, 1, 1) - 1;
                        if let Some(r) = day_range(start, end) {
                            ranges.push(r);
                        }
                        i += 2;
                    }
                    "summer" => {
                        if let Some(r) = last_season(6, 8) {
                            ranges.push(r);
                        }
                        i += 2;
                    }
                    "spring" => {
                        if let Some(r) = last_season(3, 5) {
                            ranges.push(r);
                        }
                        i += 2;
                    }
                    "fall" | "autumn" => {
                        if let Some(r) = last_season(9, 11) {
                            ranges.push(r);
                        }
                        i += 2;
                    }
                    "winter" => {
                        if let Some(r) = last_season(12, 2) {
                            ranges.push(r);
                        }
                        i += 2;
                    }
                    _ => {
                        i += 1;
                    }
                },
                "ago" => {
                    // Look back two tokens for "N unit ago" or "a unit ago".
                    if i >= 2 {
                        let unit = tokens[i - 1];
                        let count_token = tokens[i - 2];
                        let n_val: Option<i64> = if count_token == "a" || count_token == "an" {
                            Some(1)
                        } else {
                            count_token
                                .parse::<i64>()
                                .ok()
                                .filter(|&v| (1..=99).contains(&v))
                        };
                        if let Some(n_val) = n_val {
                            match unit {
                                "days" | "day" => {
                                    let target = now_days - n_val;
                                    if let Some(r) = day_range(target, target) {
                                        ranges.push(r);
                                    }
                                }
                                "weeks" | "week" => {
                                    let center = now_days - n_val * 7;
                                    if let Some(r) = day_range(center - 3, center + 3) {
                                        ranges.push(r);
                                    }
                                }
                                "months" | "month" => {
                                    // Approximate: step back n*30 days, snap to calendar month.
                                    let approx = now_days - n_val * 30;
                                    let (y, m, _) = civil_from_days(approx);
                                    if (1970..=2200).contains(&y) {
                                        let m = m.clamp(1, 12);
                                        if let Some(r) = cal_month_range(y, m) {
                                            ranges.push(r);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    i += 1;
                }
                _ => {
                    i += 1;
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
                let days = distance as f64 / DAY_MS as f64;
                (-days / crate::mechanics::priors::TEMPORAL_PROXIMITY_DECAY_DAYS).exp()
            }
        })
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAY_8_2023: u64 = 1_683_504_000_000; // 2023-05-08 00:00 UTC (ms)

    #[test]
    fn ms_native_yesterday_proximity() {
        // Regression (#3): the temporal engine must be millisecond-native so a
        // relative cue like "yesterday" matches a ms-scale node timestamp on the
        // real query path (callers pass ms). Fails today (~0) due to s/ms mismatch.
        let now = 1_720_000_000_000u64; // fixed ms epoch
        let cues = parse_time_cues("what happened yesterday", now);
        let node_ts = now - 86_400_000; // 1 day ago in MS
        let score = temporal_proximity(node_ts, &cues);
        assert!(
            score > 0.5,
            "ms-scale 'yesterday' node must score high, got {score}"
        );
    }

    #[test]
    fn resolves_relative_evidence_expressions_against_observation_day() {
        let june_6 = day_epoch(2023, 6, 6).unwrap();
        let resolutions = resolve_relative_time_cues("I did yoga yesterday morning.", june_6);
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].phrase, "yesterday");
        assert_eq!(
            resolutions[0].range,
            day_range(days_from_civil(2023, 6, 5)).unwrap()
        );
        assert_eq!(resolutions[0].precision, RelativeTimePrecision::Day);

        let may_11 = day_epoch(2023, 5, 11).unwrap();
        let resolutions = resolve_relative_time_cues("The course takes place next month.", may_11);
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].range, month_range(2023, 6).unwrap());
        assert_eq!(resolutions[0].precision, RelativeTimePrecision::Month);

        let october_21 = day_epoch(2022, 10, 21).unwrap();
        let resolutions = resolve_relative_time_cues("My mother visited two days ago.", october_21);
        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].range,
            day_range(days_from_civil(2022, 10, 19)).unwrap()
        );

        let august_17 = day_epoch(2023, 8, 17).unwrap();
        let resolutions =
            resolve_relative_time_cues("I bought the console on the 17th.", august_17);
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0].phrase, "the 17th");
        assert_eq!(
            resolutions[0].range,
            day_range(days_from_civil(2023, 8, 17)).unwrap()
        );
    }

    #[test]
    fn last_weekday_is_strictly_before_observation_day() {
        let friday_june_24 = day_epoch(2022, 6, 24).unwrap();
        let resolutions = resolve_relative_time_cues("We met last Friday.", friday_june_24);
        assert_eq!(resolutions.len(), 1);
        assert_eq!(
            resolutions[0].range,
            day_range(days_from_civil(2022, 6, 17)).unwrap()
        );
    }

    #[test]
    fn parses_natural_language_date() {
        let cues = parse_time_cues("what happened on 8 May 2023", 0);
        assert_eq!(cues.len(), 1, "expected one cue, got {cues:?}");
        assert_eq!(
            cues[0].start, MAY_8_2023,
            "start must be 2023-05-08 00:00 UTC"
        );
        assert_eq!(
            cues[0].end,
            MAY_8_2023 + DAY_MS - 1,
            "end must be end of day"
        );
    }

    #[test]
    fn parses_iso_date() {
        let cues = parse_time_cues("event on 2023-05-08 was notable", 0);
        assert_eq!(cues.len(), 1, "expected one cue");
        assert_eq!(cues[0].start, MAY_8_2023);
    }

    #[test]
    fn parses_month_year_range() {
        let cues = parse_time_cues("events in May 2023", 0);
        assert_eq!(cues.len(), 1, "expected one cue for month range");
        // May 2023: 2023-05-01 00:00 UTC = 1682899200 s = 1682899200000 ms
        assert_eq!(cues[0].start, 1_682_899_200_000, "start must be 2023-05-01");
        // End must be 2023-05-31 23:59:59.999 = 1685577599999 ms
        assert_eq!(
            cues[0].end, 1_685_577_599_999,
            "end must be last millisecond of May 2023"
        );
    }

    #[test]
    fn parses_ordinal_weekend_as_a_narrow_calendar_range() {
        let first = parse_time_cues("during the first weekend of August 2023", 0);
        assert_eq!(
            first,
            vec![TimeRange {
                start: day_epoch(2023, 8, 5).expect("first Saturday"),
                end: day_epoch(2023, 8, 7).expect("day after first weekend") - 1,
            }]
        );

        let third = parse_time_cues("on the third weekend in September 2023", 0);
        assert_eq!(
            third,
            vec![TimeRange {
                start: day_epoch(2023, 9, 16).expect("third Saturday"),
                end: day_epoch(2023, 9, 18).expect("day after third weekend") - 1,
            }]
        );

        let month = parse_time_cues("during August 2023", 0);
        assert_eq!(month, vec![month_range(2023, 8).expect("month range")]);
    }

    #[test]
    fn no_dates_returns_empty() {
        let cues = parse_time_cues("beach trip planning notes", 0);
        assert!(cues.is_empty(), "expected no cues, got {cues:?}");
    }

    #[test]
    fn bare_number_produces_nothing() {
        let cues = parse_time_cues("8 items in list", 0);
        assert!(
            cues.is_empty(),
            "bare number without context must not produce cue"
        );
    }

    #[test]
    fn standalone_year_produces_a_calendar_range() {
        let cues = parse_time_cues("something in 2023", 0);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, day_epoch(2023, 1, 1).expect("year start"));
        assert_eq!(cues[0].end, day_epoch(2024, 1, 1).expect("year end") - 1);
    }

    #[test]
    fn proximity_one_inside_range() {
        let range = TimeRange {
            start: MAY_8_2023,
            end: MAY_8_2023 + DAY_MS - 1,
        };
        let score = temporal_proximity(MAY_8_2023 + 3_600_000, &[range]);
        assert_eq!(score, 1.0, "inside range must score 1.0");
    }

    #[test]
    fn proximity_decays_outside_range() {
        let range = TimeRange {
            start: MAY_8_2023,
            end: MAY_8_2023 + DAY_MS - 1,
        };
        // 7 days before — should decay by exp(-7/7) = exp(-1)
        let ts = MAY_8_2023 - 7 * DAY_MS;
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
            end: MAY_8_2023 + DAY_MS - 1,
        };
        assert_eq!(temporal_proximity(0, &[range]), 0.0);
    }

    #[test]
    fn proximity_zero_for_empty_cues() {
        assert_eq!(temporal_proximity(MAY_8_2023, &[]), 0.0);
    }

    // ---- civil_from_days round-trips ----

    #[test]
    fn civil_roundtrip_ordinary() {
        for &(y, m, d) in &[
            (2023i64, 5u32, 8u32),
            (2023, 9, 15),
            (2023, 1, 1),
            (2023, 12, 31),
            (1970, 1, 1),
        ] {
            let days = days_from_civil(y, m, d);
            assert_eq!(
                civil_from_days(days),
                (y, m, d),
                "round-trip failed for {y}-{m:02}-{d:02}"
            );
        }
    }

    #[test]
    fn civil_roundtrip_leap_day() {
        let days = days_from_civil(2024, 2, 29);
        assert_eq!(civil_from_days(days), (2024, 2, 29));
    }

    #[test]
    fn parse_time_cues_now_zero_explicit_still_works() {
        // now=0 must not break existing explicit-date parsing.
        let cues = parse_time_cues("event on 2023-05-08 was notable", 0);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, MAY_8_2023);
    }

    #[test]
    fn parse_time_cues_now_nonzero_explicit_still_works() {
        // now != 0 must still handle explicit ISO dates.
        let now_sept_15 = 1_694_736_000_000u64; // 2023-09-15 00:00 UTC (ms)
        let cues = parse_time_cues("event on 2023-05-08 was notable", now_sept_15);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start, MAY_8_2023);
    }

    #[test]
    fn standalone_year_uses_the_calendar_year_without_widening_months() {
        let year = parse_time_cues("health incidents during 2023", 0);
        assert_eq!(year.len(), 1);
        assert_eq!(year[0].start, day_epoch(2023, 1, 1).expect("year start"));
        assert_eq!(year[0].end, day_epoch(2024, 1, 1).expect("year end") - 1);

        let month = parse_time_cues("health incidents in May 2023", 0);
        assert_eq!(month.len(), 1, "a month-qualified year stays month-sized");
        assert_eq!(month[0], month_range(2023, 5).expect("month range"));

        let day = parse_time_cues("activity on March 16, 2022", 0);
        assert_eq!(day.len(), 1, "a day-qualified year stays day-sized");
        assert_eq!(
            day[0],
            calendar_day_range(2022, 3, 16).expect("calendar day")
        );
    }

    #[test]
    fn standalone_year_does_not_guess_open_ended_boundaries() {
        assert!(parse_time_cues("events before 2023", 0).is_empty());
        assert!(parse_time_cues("events after 2023", 0).is_empty());
    }

    // ---- relative time cues ----

    // now = 2023-09-15 00:00 UTC (ms)
    const NOW_SEPT_15_2023: u64 = 1_694_736_000_000;

    #[test]
    fn relative_yesterday() {
        // yesterday relative to 2023-09-15 = 2023-09-14
        let cues = parse_time_cues("what happened yesterday", NOW_SEPT_15_2023);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let sept_14 = day_epoch(2023, 9, 14).unwrap();
        assert_eq!(cues[0].start, sept_14);
        assert_eq!(cues[0].end, sept_14 + DAY_MS - 1);
    }

    #[test]
    fn relative_last_week() {
        // last week = 7 days ending the day before now = [2023-09-08, 2023-09-14]
        let cues = parse_time_cues("what happened last week", NOW_SEPT_15_2023);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let sept_8 = day_epoch(2023, 9, 8).unwrap();
        let sept_14_end = day_epoch(2023, 9, 14).unwrap() + DAY_MS - 1;
        assert_eq!(cues[0].start, sept_8);
        assert_eq!(cues[0].end, sept_14_end);
    }

    #[test]
    fn relative_last_month() {
        // last month relative to 2023-09-15 = August 2023
        let cues = parse_time_cues("last month's events", NOW_SEPT_15_2023);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let aug_start = day_epoch(2023, 8, 1).unwrap();
        let aug_end = day_epoch(2023, 9, 1).unwrap() - 1;
        assert_eq!(cues[0].start, aug_start);
        assert_eq!(cues[0].end, aug_end);
    }

    #[test]
    fn relative_last_year() {
        // last year relative to 2023-09-15 = 2022
        let cues = parse_time_cues("what happened last year", NOW_SEPT_15_2023);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let yr_start = day_epoch(2022, 1, 1).unwrap();
        let yr_end = day_epoch(2023, 1, 1).unwrap() - 1;
        assert_eq!(cues[0].start, yr_start);
        assert_eq!(cues[0].end, yr_end);
    }

    #[test]
    fn relative_n_days_ago() {
        // "2 days ago" from 2023-09-15 = 2023-09-13
        let cues = parse_time_cues("what happened 2 days ago", NOW_SEPT_15_2023);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let sept_13 = day_epoch(2023, 9, 13).unwrap();
        assert_eq!(cues[0].start, sept_13);
        assert_eq!(cues[0].end, sept_13 + DAY_MS - 1);
    }

    #[test]
    fn relative_n_weeks_ago() {
        // "2 weeks ago" from 2023-09-15: center = now_day - 14 days = 2023-09-01
        // window = [2023-08-29, 2023-09-04]  (center ± 3 days)
        let cues = parse_time_cues("what happened 2 weeks ago", NOW_SEPT_15_2023);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let aug_29 = day_epoch(2023, 8, 29).unwrap();
        let sept_4_end = day_epoch(2023, 9, 4).unwrap() + DAY_MS - 1;
        assert_eq!(cues[0].start, aug_29);
        assert_eq!(cues[0].end, sept_4_end);
    }

    #[test]
    fn relative_a_week_ago() {
        // "a week ago" = 1 week ago: center = now - 7 = 2023-09-08
        // window = [2023-09-05, 2023-09-11]
        let cues = parse_time_cues("what happened a week ago", NOW_SEPT_15_2023);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let sept_5 = day_epoch(2023, 9, 5).unwrap();
        let sept_11_end = day_epoch(2023, 9, 11).unwrap() + DAY_MS - 1;
        assert_eq!(cues[0].start, sept_5);
        assert_eq!(cues[0].end, sept_11_end);
    }

    #[test]
    fn relative_a_month_ago() {
        // "a month ago" = 1 month ago: now - 30 days = 2023-08-16 → August 2023
        let cues = parse_time_cues("a month ago we planned", NOW_SEPT_15_2023);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let aug_start = day_epoch(2023, 8, 1).unwrap();
        let aug_end = day_epoch(2023, 9, 1).unwrap() - 1;
        assert_eq!(cues[0].start, aug_start);
        assert_eq!(cues[0].end, aug_end);
    }

    #[test]
    fn relative_last_summer_after_summer_ends() {
        // now = 2023-09-15: summer 2023 ended 2023-08-31, so last summer = 2023 summer
        let cues = parse_time_cues("What did we plan last summer?", NOW_SEPT_15_2023);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let summer_start = day_epoch(2023, 6, 1).unwrap();
        let summer_end = day_epoch(2023, 8, 31).unwrap() + DAY_MS - 1;
        assert_eq!(cues[0].start, summer_start, "summer start mismatch");
        assert_eq!(cues[0].end, summer_end, "summer end mismatch");
    }

    #[test]
    fn relative_last_summer_during_summer() {
        // now = 2023-07-10: we're inside summer 2023, so last summer = summer 2022
        let now_july_10 = day_epoch(2023, 7, 10).unwrap();
        let cues = parse_time_cues("last summer we went hiking", now_july_10);
        assert_eq!(cues.len(), 1, "expected 1 cue, got {cues:?}");
        let summer_start_2022 = day_epoch(2022, 6, 1).unwrap();
        let summer_end_2022 = day_epoch(2022, 8, 31).unwrap() + DAY_MS - 1;
        assert_eq!(cues[0].start, summer_start_2022);
        assert_eq!(cues[0].end, summer_end_2022);
    }

    #[test]
    fn relative_now_zero_skips_relative_cues() {
        // now=0 → relative cues yield nothing, but explicit dates still parse.
        let cues = parse_time_cues("yesterday we went hiking", 0);
        assert!(
            cues.is_empty(),
            "now=0 must skip relative cues, got {cues:?}"
        );

        let cues2 = parse_time_cues("event on 2023-05-08 yesterday", 0);
        assert_eq!(cues2.len(), 1, "explicit date must still parse with now=0");
        assert_eq!(cues2[0].start, MAY_8_2023);
    }
}
