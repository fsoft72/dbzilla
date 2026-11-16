//! Fuzzy and "contains" matching for the sidebar search and Open Anything (SPEC 7.3, 7.7).

/// Bonus for a character matched right after the previous match.
const CONSECUTIVE_BONUS: i32 = 15;
/// Bonus for a match at a word start.
const WORD_START_BONUS: i32 = 20;
/// Base score of a matched character.
const MATCH_SCORE: i32 = 10;
/// Bonus when the first character matches the first character of the text.
const PREFIX_BONUS: i32 = 10;

/// Matching strategy of the search boxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    #[default]
    Fuzzy,
    #[allow(dead_code)] // selectable once the search settings expose it
    Contains,
}

/// True when `text[i]` starts a word (after a separator or a lower to upper change).
fn word_start(chars: &[char], i: usize) -> bool {
    i == 0 || matches!(chars[i - 1], '_' | '-' | ' ' | '.' | '/') || (chars[i - 1].is_lowercase() && chars[i].is_uppercase())
}

/// Subsequence match; higher scores are better, `None` when `query` is not a subsequence.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().collect();
    let lower: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    if lower.len() != t.len() {
        return contains_score(query, text);
    }
    let (mut qi, mut score, mut last): (usize, i32, Option<usize>) = (0, 0, None);
    for (i, c) in lower.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            score += MATCH_SCORE;
            if last == Some(i.wrapping_sub(1)) && i > 0 {
                score += CONSECUTIVE_BONUS;
            }
            if word_start(&t, i) {
                score += WORD_START_BONUS;
            }
            if i == 0 {
                score += PREFIX_BONUS;
            }
            if let Some(l) = last {
                score -= (i - l - 1).min(5) as i32;
            }
            last = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(score)
}

/// Plain substring match; earlier matches score higher.
pub fn contains_score(query: &str, text: &str) -> Option<i32> {
    let (q, t) = (query.to_lowercase(), text.to_lowercase());
    t.find(&q).map(|pos| 100 - pos.min(99) as i32)
}

/// Matches with anchors: `^prefix`, `suffix$`, `^exact$`; otherwise the given strategy.
pub fn match_item(query: &str, text: &str, strategy: Strategy) -> Option<i32> {
    let q = query.trim();
    if q.is_empty() {
        return Some(0);
    }
    let (starts, ends) = (q.starts_with('^'), q.ends_with('$') && q.len() > 1);
    let core = q.trim_start_matches('^').trim_end_matches('$').to_lowercase();
    let t = text.to_lowercase();
    match (starts, ends) {
        (true, true) => (t == core).then_some(1000),
        (true, false) => t.starts_with(&core).then_some(500),
        (false, true) => t.ends_with(&core).then_some(400),
        (false, false) => match strategy {
            Strategy::Fuzzy => fuzzy_score(q, text),
            Strategy::Contains => contains_score(q, text),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Subsequences match, other strings do not, case is ignored.
    #[test]
    fn subsequence_matching() {
        assert!(fuzzy_score("ord", "orders").is_some());
        assert!(fuzzy_score("cmr", "customers").is_some());
        assert!(fuzzy_score("ORD", "orders").is_some());
        assert_eq!(fuzzy_score("xyz", "orders"), None);
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    /// Prefix, word-start and consecutive matches rank higher.
    #[test]
    fn ranking() {
        let order_items = fuzzy_score("oi", "order_items").unwrap();
        let loose = fuzzy_score("oi", "bonuses_info").unwrap();
        assert!(order_items > loose);
        assert!(fuzzy_score("ord", "orders").unwrap() > fuzzy_score("ord", "reorder").unwrap());
    }

    /// Anchors restrict where the text must match.
    #[test]
    fn anchors() {
        assert!(match_item("^ord", "orders", Strategy::Fuzzy).is_some());
        assert!(match_item("^ord", "reorder", Strategy::Fuzzy).is_none());
        assert!(match_item("ers$", "orders", Strategy::Fuzzy).is_some());
        assert!(match_item("ers$", "orders_old", Strategy::Fuzzy).is_none());
        assert!(match_item("^orders$", "Orders", Strategy::Fuzzy).is_some());
        assert!(match_item("^orders$", "orders_old", Strategy::Fuzzy).is_none());
    }

    /// The contains strategy needs a real substring.
    #[test]
    fn contains_strategy() {
        assert!(match_item("rde", "orders", Strategy::Contains).is_some());
        assert!(match_item("ods", "orders", Strategy::Contains).is_none());
        assert!(match_item("ods", "orders", Strategy::Fuzzy).is_some());
    }

    /// Unicode text does not panic.
    #[test]
    fn unicode_is_safe() {
        assert!(fuzzy_score("è", "città_è").is_some());
        assert!(fuzzy_score("İ", "i").is_none() || fuzzy_score("İ", "i").is_some());
    }
}
