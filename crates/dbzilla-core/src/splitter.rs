//! Dialect-aware SQL statement splitter (strings, comments, `$$` bodies, MySQL `DELIMITER`).

use crate::dialect::DriverId;

/// Label prefixes recognised in a comment directly above a statement.
const LABEL_PREFIXES: [&str; 3] = ["name:", "label:", "tab:"];

/// One statement found in a script.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// Statement text, trimmed, including leading comments.
    pub text: String,
    /// Byte offset of the first character in the source.
    pub start: usize,
    /// Byte offset one past the last character in the source.
    pub end: usize,
    /// Result tab name taken from a `-- name:` / `label:` / `tab:` comment above.
    pub label: Option<String>,
}

/// Splits `sql` into statements for the given dialect. Never panics on malformed input.
pub fn split_statements(sql: &str, dialect: DriverId) -> Vec<Statement> {
    let b = sql.as_bytes();
    let mysql = dialect == DriverId::MySql;
    let postgres = dialect == DriverId::Postgres;
    let mut out = Vec::new();
    let mut delim: Vec<u8> = vec![b';'];
    let mut chunk_start = 0usize;
    let mut has_code = false;
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if mysql && !has_code && at_line_start(b, i) {
            if let Some((new_delim, next)) = parse_delimiter_line(sql, i) {
                delim = new_delim;
                i = next;
                chunk_start = i;
                continue;
            }
        }
        if b[i..].starts_with(&delim) {
            push_statement(sql, chunk_start, i, has_code, &mut out);
            i += delim.len();
            chunk_start = i;
            has_code = false;
            continue;
        }
        match c {
            b'\'' | b'"' | b'`' => {
                has_code = true;
                i = skip_quoted(b, i, c, mysql);
            }
            b'-' if b.get(i + 1) == Some(&b'-') => i = skip_line(b, i),
            b'#' if mysql => i = skip_line(b, i),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                // MySQL executable comments (/*! ... */) are code, not comments
                if mysql && b.get(i + 2) == Some(&b'!') {
                    has_code = true;
                }
                i = skip_block_comment(b, i);
            }
            b'$' if postgres => {
                has_code = true;
                i = skip_dollar_quote(b, i);
            }
            c if c.is_ascii_whitespace() => i += 1,
            _ => {
                has_code = true;
                i += 1;
            }
        }
    }
    push_statement(sql, chunk_start, b.len(), has_code, &mut out);
    out
}

/// True when `i` is at the start of a line (ignoring leading blanks on that line).
fn at_line_start(b: &[u8], i: usize) -> bool {
    let mut j = i;
    while j > 0 && (b[j - 1] == b' ' || b[j - 1] == b'\t') {
        j -= 1;
    }
    j == 0 || b[j - 1] == b'\n'
}

/// Parses a MySQL `DELIMITER xx` line starting at `i`; returns the delimiter and the next offset.
fn parse_delimiter_line(sql: &str, i: usize) -> Option<(Vec<u8>, usize)> {
    let rest = &sql[i..];
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let line = rest[..line_end].trim();
    let head = line.get(..9)?;
    if !head.eq_ignore_ascii_case("DELIMITER") || !line[9..].starts_with(char::is_whitespace) {
        return None;
    }
    let new_delim = line[9..].trim();
    if new_delim.is_empty() {
        return None;
    }
    Some((new_delim.as_bytes().to_vec(), i + line_end))
}

/// Skips a quoted region starting at `i`; returns the offset after the closing quote.
fn skip_quoted(b: &[u8], i: usize, q: u8, backslash: bool) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        if backslash && b[j] == b'\\' {
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

/// Skips to the end of the current line (the newline itself is left in place).
fn skip_line(b: &[u8], i: usize) -> usize {
    b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p)
}

/// Skips a block comment starting at `i`.
fn skip_block_comment(b: &[u8], i: usize) -> usize {
    let mut j = i + 2;
    while j + 1 < b.len() {
        if b[j] == b'*' && b[j + 1] == b'/' {
            return j + 2;
        }
        j += 1;
    }
    b.len()
}

/// Skips a PostgreSQL dollar-quoted string if one starts at `i`, else just the `$`.
fn skip_dollar_quote(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    let tag_ok = b.get(j) == Some(&b'$') && !b.get(i + 1).is_some_and(u8::is_ascii_digit);
    if !tag_ok {
        return i + 1;
    }
    let tag = &b[i..=j];
    let mut k = j + 1;
    while k + tag.len() <= b.len() {
        if &b[k..k + tag.len()] == tag {
            return k + tag.len();
        }
        k += 1;
    }
    b.len()
}

/// Appends the statement `sql[from..to]` when it contains code.
fn push_statement(sql: &str, from: usize, to: usize, has_code: bool, out: &mut Vec<Statement>) {
    if !has_code {
        return;
    }
    let raw = &sql[from..to];
    let lead = raw.len() - raw.trim_start().len();
    let text = raw.trim();
    out.push(Statement { text: text.to_owned(), start: from + lead, end: from + lead + text.len(), label: extract_label(text) });
}

/// Finds the last `-- name:|label:|tab:` directive among the comment lines above the statement.
fn extract_label(text: &str) -> Option<String> {
    let mut label = None;
    for line in text.lines() {
        let Some(comment) = line.trim_start().strip_prefix("--") else { break };
        let comment = comment.trim();
        let lower = comment.to_ascii_lowercase();
        for prefix in LABEL_PREFIXES {
            if lower.starts_with(prefix) {
                let value = comment[prefix.len()..].trim();
                if !value.is_empty() {
                    label = Some(value.to_owned());
                }
            }
        }
    }
    label
}
