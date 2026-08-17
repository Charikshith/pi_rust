//! Port of `packages/tui/src/fuzzy.ts` — fuzzy matching: all query characters
//! must appear in `text` in order (not necessarily consecutively); lower
//! score = better match. See `docs/analysis/05-tui.md` §2/§9.
//!
//! ## Scope decision (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **`char`, not UTF-16-code-unit, indexing.** The TS source indexes
//!   `textLower[i]`/`queryLower[i]` by UTF-16 code unit (JS string indexing);
//!   this port iterates by Rust `char` (Unicode scalar value). The two only
//!   diverge for astral-plane (>U+FFFF) input, where a single `char` is one
//!   index here but two JS code units there — this would shift `i`-derived
//!   score contributions (word-boundary/gap/position penalties) for text
//!   containing such characters. Fuzzy-matched text in Pi is command names,
//!   file paths, and setting labels — realistically ASCII/BMP — so this is a
//!   documented, low-stakes simplification rather than a byte-exact
//!   requirement (this is a search-ranking heuristic, not a persisted format).

/// `FuzzyMatch` (fuzzy.ts:7).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    pub matches: bool,
    pub score: f64,
}

/// Boundary characters for the "reward word-boundary matches" bonus
/// (`/[\s\-_./:]/`, fuzzy.ts:32).
fn is_boundary_char(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '_' | '.' | '/' | ':')
}

fn match_query(query: &[char], text: &[char]) -> FuzzyMatch {
    if query.is_empty() {
        return FuzzyMatch {
            matches: true,
            score: 0.0,
        };
    }
    if query.len() > text.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    let mut query_index = 0usize;
    let mut score = 0.0f64;
    let mut last_match_index: i64 = -1;
    let mut consecutive_matches: i64 = 0;

    for (i, &tc) in text.iter().enumerate() {
        if query_index >= query.len() {
            break;
        }
        if tc == query[query_index] {
            let is_word_boundary = i == 0 || is_boundary_char(text[i - 1]);

            if last_match_index == i as i64 - 1 {
                consecutive_matches += 1;
                score -= (consecutive_matches * 5) as f64;
            } else {
                consecutive_matches = 0;
                if last_match_index >= 0 {
                    score += (i as i64 - last_match_index - 1) as f64 * 2.0;
                }
            }

            if is_word_boundary {
                score -= 10.0;
            }
            score += i as f64 * 0.1;

            last_match_index = i as i64;
            query_index += 1;
        }
    }

    if query_index < query.len() {
        return FuzzyMatch {
            matches: false,
            score: 0.0,
        };
    }

    if query.iter().eq(text.iter()) {
        score -= 100.0;
    }

    FuzzyMatch {
        matches: true,
        score,
    }
}

/// Finds the `^[a-z]+[0-9]+$` / `^[0-9]+[a-z]+$` transposition of `s`, if `s`
/// matches either shape entirely (mirrors fuzzy.ts:75-81's named-capture
/// regex split, hand-scanned per the Ponytail ladder rather than pulling in a
/// regex crate for two fixed ASCII character classes).
fn swap_alpha_numeric(s: &[char]) -> Option<Vec<char>> {
    fn split(chars: &[char], first_is_letters: bool) -> Option<(Vec<char>, Vec<char>)> {
        if chars.is_empty() {
            return None;
        }
        let first_class = |c: &char| {
            if first_is_letters {
                c.is_ascii_lowercase()
            } else {
                c.is_ascii_digit()
            }
        };
        let mut i = 0;
        while i < chars.len() && first_class(&chars[i]) {
            i += 1;
        }
        if i == 0 || i == chars.len() {
            return None;
        }
        let second_class = |c: &char| {
            if first_is_letters {
                c.is_ascii_digit()
            } else {
                c.is_ascii_lowercase()
            }
        };
        if !chars[i..].iter().all(second_class) {
            return None;
        }
        Some((chars[..i].to_vec(), chars[i..].to_vec()))
    }

    if let Some((letters, digits)) = split(s, true) {
        let mut swapped = digits;
        swapped.extend(letters);
        return Some(swapped);
    }
    if let Some((digits, letters)) = split(s, false) {
        let mut swapped = letters;
        swapped.extend(digits);
        return Some(swapped);
    }
    None
}

/// `fuzzyMatch` (fuzzy.ts:12).
pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower: Vec<char> = query.to_lowercase().chars().collect();
    let text_lower: Vec<char> = text.to_lowercase().chars().collect();

    let primary = match_query(&query_lower, &text_lower);
    if primary.matches {
        return primary;
    }

    let Some(swapped_query) = swap_alpha_numeric(&query_lower) else {
        return primary;
    };

    let swapped = match_query(&swapped_query, &text_lower);
    if !swapped.matches {
        return primary;
    }

    FuzzyMatch {
        matches: true,
        score: swapped.score + 5.0,
    }
}

/// Filter and sort items by fuzzy match quality, best matches first
/// (`fuzzyFilter`, fuzzy.ts:99). Supports whitespace- and slash-separated
/// query tokens: all tokens must match. Returns borrowed items (the TS
/// returns the same object references) rather than requiring `T: Clone`.
pub fn fuzzy_filter<'a, T>(
    items: &'a [T],
    query: &str,
    get_text: impl Fn(&T) -> String,
) -> Vec<&'a T> {
    if query.trim().is_empty() {
        return items.iter().collect();
    }

    let tokens: Vec<&str> = query
        .trim()
        .split(|c: char| c.is_whitespace() || c == '/')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return items.iter().collect();
    }

    let mut results: Vec<(&T, f64)> = Vec::new();
    for item in items {
        let text = get_text(item);
        let mut total_score = 0.0f64;
        let mut all_match = true;
        for token in &tokens {
            let m = fuzzy_match(token, &text);
            if m.matches {
                total_score += m.score;
            } else {
                all_match = false;
                break;
            }
        }
        if all_match {
            results.push((item, total_score));
        }
    }

    results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    results.into_iter().map(|(item, _)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_scores_lowest() {
        let m = fuzzy_match("hello", "hello");
        assert!(m.matches);
        assert!(m.score < 0.0);
    }

    #[test]
    fn missing_char_does_not_match() {
        assert!(!fuzzy_match("xyz", "hello").matches);
    }

    #[test]
    fn query_longer_than_text_does_not_match() {
        assert!(!fuzzy_match("helloworld", "hi").matches);
    }

    #[test]
    fn empty_query_always_matches() {
        let m = fuzzy_match("", "anything");
        assert!(m.matches);
        assert_eq!(m.score, 0.0);
    }

    #[test]
    fn alpha_numeric_swap_fires_when_primary_fails() {
        let m = fuzzy_match("2fa", "fa2-setup");
        assert!(m.matches);
    }

    #[test]
    fn fuzzy_filter_empty_query_returns_all_items_unchanged() {
        let items = vec!["a".to_string(), "b".to_string()];
        let result = fuzzy_filter(&items, "  ", |s| s.clone());
        assert_eq!(result, vec![&items[0], &items[1]]);
    }

    #[test]
    fn fuzzy_filter_requires_all_tokens_to_match() {
        let items = vec!["apple pie".to_string(), "banana split".to_string()];
        let result = fuzzy_filter(&items, "apple pie", |s| s.clone());
        assert_eq!(result, vec![&items[0]]);
    }
}
