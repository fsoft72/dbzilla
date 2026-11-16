//! What is shown in the console log: redacted and shortened statements (SPEC 7.5, NFR-05).

/// Statements longer than this are replaced by a short note.
pub const MAX_CONSOLE_STATEMENT: usize = 10_000;
/// Mask written in place of secrets.
const MASK: &str = "'***'";

/// True when `b` can be part of an identifier word.
fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// End offset (exclusive) of the string literal starting at `start`.
fn literal_end(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'\'' if b.get(i + 1) == Some(&b'\'') => i += 2,
            b'\'' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// Advances `j` past spaces.
fn skip_blanks(b: &[u8], j: &mut usize) {
    while *j < b.len() && b[*j] == b' ' {
        *j += 1;
    }
}

/// Length of a secret-introducing keyword at `i`, if any.
fn keyword_len(lower: &str, b: &[u8], i: usize) -> Option<usize> {
    let rest = &lower[i..];
    let len = if rest.starts_with("password") {
        8
    } else if rest.starts_with("identified by") {
        13
    } else {
        return None;
    };
    let before_ok = i == 0 || !is_word(b[i - 1]);
    let after_ok = b.get(i + len).is_none_or(|c| !is_word(*c));
    (before_ok && after_ok).then_some(len)
}

/// Replaces the string literal following `PASSWORD` or `IDENTIFIED BY` with a mask.
pub fn redact_sql(sql: &str) -> String {
    let lower = sql.to_ascii_lowercase();
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < sql.len() {
        if b[i] == b'\'' {
            let end = literal_end(b, i);
            out.push_str(&sql[i..end]);
            i = end;
            continue;
        }
        if let Some(k) = keyword_len(&lower, b, i) {
            out.push_str(&sql[i..i + k]);
            let mut j = i + k;
            skip_blanks(b, &mut j);
            for optional in b"=(" {
                if b.get(j) == Some(optional) {
                    j += 1;
                    skip_blanks(b, &mut j);
                }
            }
            out.push_str(&sql[i + k..j]);
            if j < b.len() && b[j] == b'\'' {
                out.push_str(MASK);
                i = literal_end(b, j);
            } else {
                i = j;
            }
            continue;
        }
        let ch = sql[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Text to show in the console for a statement.
pub fn display_for_console(sql: &str) -> String {
    if sql.len() > MAX_CONSOLE_STATEMENT {
        return format!("-- statement of {} bytes (not shown)", sql.len());
    }
    redact_sql(sql)
}
