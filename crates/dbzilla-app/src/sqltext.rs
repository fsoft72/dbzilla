//! Text operations of the SQL editor: statement lookup, formatting, comments, find (SPEC 10.3, 10.6).

use dbzilla_core::dialect::DriverId;
use dbzilla_core::splitter::{split_statements, Statement};
use dbzilla_store::settings::{IndentType, IndentWith};
use sqlformat::{FormatOptions, Indent, QueryParams};

/// Comment prefix toggled by the editor.
const COMMENT_PREFIX: &str = "-- ";

/// The statement under the cursor: the one containing it, else the closest one before it.
pub fn statement_at(sql: &str, cursor: usize, dialect: DriverId) -> Option<Statement> {
    let all = split_statements(sql, dialect);
    all.iter().find(|s| s.start <= cursor && cursor <= s.end).or_else(|| all.iter().rev().find(|s| s.end <= cursor)).or_else(|| all.first()).cloned()
}

/// Formats SQL; keywords are upper-cased when `uppercase` is true.
pub fn beautify(sql: &str, width: u8, with: IndentWith, kind: IndentType, uppercase: bool) -> String {
    let indent = match with {
        IndentWith::Tab => Indent::Tabs,
        IndentWith::Spaces => Indent::Spaces(if kind == IndentType::Standard { width } else { 10 }),
    };
    let options = FormatOptions { indent, uppercase: Some(uppercase), lines_between_queries: 1, ..Default::default() };
    sqlformat::format(sql, &QueryParams::None, &options)
}

/// Collapses a statement to one line: whitespace runs become one space and comments are dropped.
pub fn minify(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    let mut pending_space = false;
    while i < b.len() {
        match b[i] {
            q @ (b'\'' | b'"' | b'`') => {
                if pending_space && !out.is_empty() {
                    out.push(' ');
                }
                pending_space = false;
                let mut j = i + 1;
                while j < b.len() {
                    if b[j] == q {
                        if b.get(j + 1) == Some(&q) {
                            j += 2;
                            continue;
                        }
                        j += 1;
                        break;
                    }
                    j += 1;
                }
                out.push_str(&sql[i..j.min(b.len())]);
                i = j;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p);
                pending_space = true;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
                pending_space = true;
            }
            c if c.is_ascii_whitespace() => {
                pending_space = true;
                i += 1;
            }
            _ => {
                if pending_space && !out.is_empty() {
                    out.push(' ');
                }
                pending_space = false;
                let ch = sql[i..].chars().next().unwrap_or(' ');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

/// Comments or uncomments the lines touched by the byte range; returns the new text and range.
pub fn toggle_line_comment(text: &str, range: (usize, usize)) -> (String, (usize, usize)) {
    let (a, b) = (range.0.min(text.len()), range.1.min(text.len()));
    let first = text[..a].rfind('\n').map_or(0, |p| p + 1);
    let last = if b > a && text.as_bytes().get(b - 1) == Some(&b'\n') { b - 1 } else { b };
    let end = text[last..].find('\n').map_or(text.len(), |p| last + p);
    let block = &text[first..end];
    let lines: Vec<&str> = block.split('\n').collect();
    let all_commented = lines.iter().filter(|l| !l.trim().is_empty()).all(|l| l.trim_start().starts_with("--"));
    let toggled: Vec<String> = lines
        .iter()
        .map(|l| {
            if l.trim().is_empty() {
                return (*l).to_owned();
            }
            let indent = l.len() - l.trim_start().len();
            if all_commented {
                let rest = &l[indent..];
                let rest = rest.strip_prefix(COMMENT_PREFIX).or_else(|| rest.strip_prefix("--")).unwrap_or(rest);
                format!("{}{rest}", &l[..indent])
            } else {
                format!("{}{COMMENT_PREFIX}{}", &l[..indent], &l[indent..])
            }
        })
        .collect();
    let new_block = toggled.join("\n");
    let mut out = String::with_capacity(text.len() + 8);
    out.push_str(&text[..first]);
    out.push_str(&new_block);
    out.push_str(&text[end..]);
    (out, (first, first + new_block.len()))
}

/// Next or previous occurrence of `needle` from `from`, wrapping around; returns the byte range.
pub fn find_next(text: &str, needle: &str, from: usize, case_sensitive: bool, forward: bool) -> Option<(usize, usize)> {
    if needle.is_empty() {
        return None;
    }
    let (hay, pat) = if case_sensitive { (text.to_owned(), needle.to_owned()) } else { (text.to_lowercase(), needle.to_lowercase()) };
    if hay.len() != text.len() {
        return None;
    }
    let from = from.min(hay.len());
    let hit = if forward {
        hay[from..].find(&pat).map(|p| p + from).or_else(|| hay.find(&pat))
    } else {
        hay[..from].rfind(&pat).or_else(|| hay.rfind(&pat))
    };
    hit.map(|s| (s, s + pat.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cursor picks the statement it is in, else the previous one.
    #[test]
    fn current_statement() {
        let sql = "SELECT 1;\nSELECT 2;\n\nSELECT 3";
        assert_eq!(statement_at(sql, 3, DriverId::Sqlite).unwrap().text, "SELECT 1");
        assert_eq!(statement_at(sql, 14, DriverId::Sqlite).unwrap().text, "SELECT 2");
        assert_eq!(statement_at(sql, 19, DriverId::Sqlite).unwrap().text, "SELECT 2", "between statements the previous one wins");
        assert_eq!(statement_at(sql, sql.len(), DriverId::Sqlite).unwrap().text, "SELECT 3");
        assert!(statement_at("  ", 0, DriverId::Sqlite).is_none());
    }

    /// Beautify puts clauses on their own lines and can upper-case keywords.
    #[test]
    fn beautify_formats() {
        let out = beautify("select a,b from t where x=1", 4, IndentWith::Spaces, IndentType::Standard, true);
        assert!(out.contains("SELECT\n    a,\n    b\nFROM\n    t\nWHERE\n    x = 1"), "{out}");
        let lower = beautify("select a from t", 2, IndentWith::Spaces, IndentType::Standard, false);
        assert!(lower.starts_with("select"), "{lower}");
    }

    /// Minify drops comments and collapses whitespace but keeps strings intact.
    #[test]
    fn minify_collapses() {
        assert_eq!(minify("SELECT  a,\n   b -- note\n FROM t /* x */ WHERE n = 'a  b'"), "SELECT a, b FROM t WHERE n = 'a  b'");
        assert_eq!(minify("  x  "), "x");
        assert_eq!(minify("SELECT 'it''s'   FROM t"), "SELECT 'it''s' FROM t");
    }

    /// Commenting then uncommenting restores the text; mixed blocks get commented.
    #[test]
    fn comment_toggle() {
        let text = "a\n  b\n\nc";
        let (commented, range) = toggle_line_comment(text, (0, 6));
        assert_eq!(commented, "-- a\n  -- b\n\nc");
        let (restored, _) = toggle_line_comment(&commented, (0, range.1));
        assert_eq!(restored, text);
        let (mixed, _) = toggle_line_comment("-- a\nb", (0, 6));
        assert_eq!(mixed, "-- -- a\n-- b");
        let (single, _) = toggle_line_comment("x\ny", (2, 2));
        assert_eq!(single, "x\n-- y");
    }

    /// Find wraps around and can go backwards, case-insensitively.
    #[test]
    fn find() {
        let t = "Select a, select b";
        assert_eq!(find_next(t, "select", 0, false, true), Some((0, 6)));
        assert_eq!(find_next(t, "select", 1, false, true), Some((10, 16)));
        assert_eq!(find_next(t, "select", 11, false, true), Some((0, 6)), "wraps");
        assert_eq!(find_next(t, "select", 10, false, false), Some((0, 6)));
        assert_eq!(find_next(t, "select", 0, true, true), Some((10, 16)), "case sensitive skips the first");
        assert_eq!(find_next(t, "", 0, false, true), None);
        assert_eq!(find_next(t, "zzz", 0, false, true), None);
    }
}
