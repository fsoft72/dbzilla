//! Row limit injection for statements typed in the SQL editor.

/// Values of the row limit menu (SPEC 10.2): no limit, then 100 up to 500,000.
pub const ROW_LIMITS: [Option<u64>; 9] = [None, Some(100), Some(500), Some(1000), Some(5000), Some(10_000), Some(50_000), Some(100_000), Some(500_000)];

/// Words at the top level of a statement that mean it already limits (or cannot take a limit).
const BLOCKERS: [&str; 4] = ["LIMIT", "FETCH", "TOP", "INTO"];

/// Skips a quoted region starting at `i` (quote character `q`); returns the offset after it.
fn skip_quoted(b: &[u8], i: usize, q: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j] == q {
            if b.get(j + 1) == Some(&q) {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    b.len()
}

/// Top-level words of a statement (lower depth only), upper-cased, ignoring strings and comments.
fn top_level_words(sql: &str) -> Vec<String> {
    let b = sql.as_bytes();
    let (mut i, mut depth, mut words) = (0, 0i32, Vec::new());
    while i < b.len() {
        match b[i] {
            b'\'' | b'"' | b'`' => i = skip_quoted(b, i, b[i]),
            b'-' if b.get(i + 1) == Some(&b'-') => i = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p),
            b'/' if b.get(i + 1) == Some(&b'*') => i = sql[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2),
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                if depth == 0 {
                    words.push(sql[start..i].to_ascii_uppercase());
                }
            }
            _ => i += 1,
        }
    }
    words
}

/// Appends `LIMIT n` to a `SELECT` (or `WITH ... SELECT`) that has no limit of its own.
/// Anything else, or `None`, returns the statement unchanged.
pub fn apply_row_limit(sql: &str, limit: Option<u64>) -> String {
    let Some(n) = limit else { return sql.to_owned() };
    let words = top_level_words(sql);
    let is_select = matches!(words.first().map(String::as_str), Some("SELECT" | "WITH"));
    if !is_select || words.iter().any(|w| BLOCKERS.contains(&w.as_str())) {
        return sql.to_owned();
    }
    let trimmed = sql.trim_end().trim_end_matches(';').trim_end();
    format!("{trimmed}\nLIMIT {n}")
}
