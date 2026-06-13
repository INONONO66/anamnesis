//! Deterministic dataset date parsing (no chrono dependency).
//!
//! LongMemEval: "2023/05/20 (Sat) 02:21"  → epoch seconds (UTC).
//! LoCoMo:      "1:56 pm on 8 May, 2023"  → epoch seconds (UTC).

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

/// Days from civil date (Howard Hinnant's algorithm), valid for year >= 1970.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = ((month + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn epoch_secs(year: i64, month: u32, day: u32, hour: u32, minute: u32) -> Option<u64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    if !(1970..=2200).contains(&year) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400 + hour as u64 * 3_600 + minute as u64 * 60)
}

/// "2023/05/20 (Sat) 02:21" (weekday part optional, time part optional).
pub fn parse_longmemeval_date(value: &str) -> Option<u64> {
    let mut date_part = None;
    let mut time_part = (0u32, 0u32);
    for token in value.split_whitespace() {
        let token =
            token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/' && c != ':');
        if token.matches('/').count() == 2 {
            let mut parts = token.split('/');
            let year: i64 = parts.next()?.parse().ok()?;
            let month: u32 = parts.next()?.parse().ok()?;
            let day: u32 = parts.next()?.parse().ok()?;
            date_part = Some((year, month, day));
        } else if token.matches(':').count() == 1 {
            let mut parts = token.split(':');
            if let (Some(Ok(h)), Some(Ok(m))) = (
                parts.next().map(str::parse::<u32>),
                parts.next().map(str::parse::<u32>),
            ) {
                time_part = (h, m);
            }
        }
    }
    let (year, month, day) = date_part?;
    epoch_secs(year, month, day, time_part.0, time_part.1)
}

/// "1:56 pm on 8 May, 2023" / "8 May 2023" (time prefix optional).
pub fn parse_locomo_date_time(value: &str) -> Option<u64> {
    let lower = value.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .collect();

    let mut hour = 0u32;
    let mut minute = 0u32;
    let mut day = None;
    let mut month = None;
    let mut year = None;

    for (index, token) in tokens.iter().enumerate() {
        if let Some(position) = MONTHS.iter().position(|m| m == token) {
            month = Some(position as u32 + 1);
            for neighbor in [index.wrapping_sub(1), index + 1] {
                if let Some(t) = tokens.get(neighbor)
                    && let Ok(d) = t.parse::<u32>()
                    && (1..=31).contains(&d)
                {
                    day = Some(d);
                }
            }
        } else if let Ok(value) = token.parse::<i64>() {
            if (1970..=2200).contains(&value) {
                year = Some(value);
            }
        } else if token.matches(':').count() == 1 {
            let mut parts = token.split(':');
            if let (Some(Ok(h)), Some(Ok(m))) = (
                parts.next().map(str::parse::<u32>),
                parts.next().map(str::parse::<u32>),
            ) {
                hour = h % 12;
                minute = m;
            }
        } else if *token == "pm" {
            hour += 12;
        }
    }
    epoch_secs(year?, month?, day?, hour, minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longmemeval_format_parses() {
        let ts = parse_longmemeval_date("2023/05/20 (Sat) 02:21").unwrap();
        // 2023-05-20 00:00 UTC = 1684540800
        assert_eq!(ts, 1_684_540_800 + 2 * 3600 + 21 * 60);
    }

    #[test]
    fn locomo_format_parses() {
        let ts = parse_locomo_date_time("1:56 pm on 8 May, 2023").unwrap();
        // 2023-05-08 00:00 UTC = 1683504000
        assert_eq!(ts, 1_683_504_000 + 13 * 3600 + 56 * 60);
    }

    #[test]
    fn locomo_noon_and_midnight_handle_12() {
        // 12:30 pm = 12:30; 12:05 am = 00:05.
        let noon = parse_locomo_date_time("12:30 pm on 8 May, 2023").unwrap();
        assert_eq!(noon, 1_683_504_000 + 12 * 3600 + 30 * 60);
        let midnight = parse_locomo_date_time("12:05 am on 8 May, 2023").unwrap();
        assert_eq!(midnight, 1_683_504_000 + 5 * 60);
    }

    #[test]
    fn garbage_returns_none() {
        assert_eq!(parse_longmemeval_date("no date here"), None);
        assert_eq!(parse_locomo_date_time("session one"), None);
    }
}
