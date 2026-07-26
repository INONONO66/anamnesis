//! Official-compatible deterministic answer metrics used by local quality runs.

use std::collections::BTreeMap;

use nltk_porter::{Mode, PorterStemmer};

/// Score one LoCoMo answer using the reference evaluator's category rules.
pub fn locomo_official_score(
    question_type: &str,
    reference: &str,
    prediction: &str,
) -> Option<f64> {
    match question_type {
        "adversarial" => {
            let normalized = prediction.to_lowercase();
            Some(f64::from(
                normalized.contains("no information available")
                    || normalized.contains("not mentioned"),
            ))
        }
        "multi-hop" => Some(multi_answer_f1(prediction, reference)),
        "open-domain" => Some(token_f1(
            prediction,
            reference.split(';').next().unwrap_or(reference).trim(),
        )),
        "temporal" | "single-hop" => Some(token_f1(prediction, reference)),
        _ => None,
    }
}

/// Canonicalize only a standalone ISO calendar date (`YYYY-MM-DD`) into the
/// natural-language form used by LoCoMo references.
///
/// This transform is deliberately reference-blind and narrow: relative
/// expressions such as `the day before 2023-06-26`, surrounding prose, invalid
/// dates, and non-date answers are returned unchanged. Its score must be
/// reported as a reader-surface diagnostic, never as the official raw F1.
pub fn canonicalize_standalone_iso_date(prediction: &str) -> String {
    let trimmed = prediction.trim();
    let bytes = trimmed.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| index != 4 && index != 7 && !byte.is_ascii_digit())
    {
        return prediction.to_string();
    }
    let Ok(year) = trimmed[0..4].parse::<u16>() else {
        return prediction.to_string();
    };
    let Ok(month) = trimmed[5..7].parse::<u8>() else {
        return prediction.to_string();
    };
    let Ok(day) = trimmed[8..10].parse::<u8>() else {
        return prediction.to_string();
    };
    let month_name = match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => return prediction.to_string(),
    };
    let max_day = match month {
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day == 0 || day > max_day {
        return prediction.to_string();
    }
    format!("{day} {month_name} {year}")
}

fn multi_answer_f1(prediction: &str, reference: &str) -> f64 {
    let predictions: Vec<_> = prediction.split(',').map(str::trim).collect();
    let references: Vec<_> = reference.split(',').map(str::trim).collect();
    if references.is_empty() {
        return 0.0;
    }
    references
        .iter()
        .map(|reference_item| {
            predictions
                .iter()
                .map(|prediction_item| token_f1(prediction_item, reference_item))
                .fold(0.0, f64::max)
        })
        .sum::<f64>()
        / references.len() as f64
}

fn token_f1(prediction: &str, reference: &str) -> f64 {
    let prediction_tokens = normalized_tokens(prediction);
    let reference_tokens = normalized_tokens(reference);
    if prediction_tokens.is_empty() || reference_tokens.is_empty() {
        return 0.0;
    }
    let mut prediction_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for token in &prediction_tokens {
        *prediction_counts.entry(token.as_str()).or_insert(0) += 1;
    }
    let mut reference_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for token in &reference_tokens {
        *reference_counts.entry(token.as_str()).or_insert(0) += 1;
    }
    let common = prediction_counts
        .iter()
        .map(|(token, count)| {
            reference_counts
                .get(token)
                .map_or(0, |other| (*count).min(*other))
        })
        .sum::<usize>();
    if common == 0 {
        return 0.0;
    }
    let precision = common as f64 / prediction_tokens.len() as f64;
    let recall = common as f64 / reference_tokens.len() as f64;
    2.0 * precision * recall / (precision + recall)
}

fn normalized_tokens(value: &str) -> Vec<String> {
    const ASCII_PUNCTUATION: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
    let normalized: String = value
        .to_lowercase()
        .chars()
        .filter(|character| !ASCII_PUNCTUATION.contains(*character))
        .collect();
    let stemmer = PorterStemmer::new(Mode::Nltk);
    normalized
        .split_whitespace()
        .filter(|token| !matches!(*token, "a" | "an" | "the" | "and"))
        .map(|token| stemmer.stem(token))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_f1_matches_official_normalization_shape() {
        assert_eq!(token_f1("The cats, and dogs.", "cat dog"), 1.0);
        assert_eq!(token_f1("June 5", "June 6"), 0.5);
        assert_eq!(token_f1("unrelated", "answer"), 0.0);
    }

    #[test]
    fn stemmer_matches_nltk_extension_mode_not_snowball() {
        let tokens = normalized_tokens("generously herring earring");
        assert_eq!(tokens, ["gener", "her", "ear"]);
    }

    #[test]
    fn multi_answer_score_averages_best_prediction_match() {
        let score = multi_answer_f1("Paris, unknown", "Paris, Rome");
        assert!((score - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn open_domain_uses_primary_reference_segment() {
        assert_eq!(
            locomo_official_score("open-domain", "photography; taking pictures", "Photography"),
            Some(1.0)
        );
    }

    #[test]
    fn adversarial_uses_official_abstention_phrases() {
        assert_eq!(
            locomo_official_score("adversarial", "", "No information available."),
            Some(1.0)
        );
        assert_eq!(
            locomo_official_score("adversarial", "", "UNKNOWN"),
            Some(0.0)
        );
    }

    #[test]
    fn canonicalizes_only_valid_standalone_iso_dates() {
        assert_eq!(
            canonicalize_standalone_iso_date("2023-04-02"),
            "2 April 2023"
        );
        assert_eq!(
            canonicalize_standalone_iso_date("2024-02-29"),
            "29 February 2024"
        );
        assert_eq!(
            canonicalize_standalone_iso_date("the day before 2023-06-26"),
            "the day before 2023-06-26"
        );
        assert_eq!(canonicalize_standalone_iso_date("2023-02-29"), "2023-02-29");
    }
}
