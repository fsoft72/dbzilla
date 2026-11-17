//! Query parameters (SPEC 10.11): finds placeholders in SQL and substitutes their values.
//!
//! Values are inserted literally (the user adds the quotes for strings). Placeholders inside
//! comments are always ignored; inside quoted strings only when `allow_in_quotes` is set.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Query parameter placeholder format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ParamFormat {
    /// `:id`
    #[default]
    Colon,
    /// `%id%`
    Percent,
    /// `?` (positional, named `?1`, `?2`, ...)
    Question,
    /// `${id}`
    DollarBraces,
    /// `$id`
    Dollar,
    /// `{{id}}`
    DoubleBraces,
}

impl ParamFormat {
    /// All formats, in the order shown in the settings.
    pub const ALL: [ParamFormat; 6] = [Self::Colon, Self::Percent, Self::Question, Self::DollarBraces, Self::Dollar, Self::DoubleBraces];

    /// Human readable label with an example.
    pub fn label(self) -> &'static str {
        match self {
            Self::Colon => "Colon (:id)",
            Self::Percent => "Percent (%id%)",
            Self::Question => "Question mark (?)",
            Self::DollarBraces => "Dollar braces (${id})",
            Self::Dollar => "Dollar ($id)",
            Self::DoubleBraces => "Double braces ({{id}})",
        }
    }
}

/// How placeholders are recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParamOptions {
    /// Placeholder format.
    pub format: ParamFormat,
    /// Also look for placeholders inside quoted strings.
    pub allow_in_quotes: bool,
}

/// A placeholder found in a SQL text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    /// Variable name (`?1`, `?2`, ... for the question mark format).
    pub name: String,
    /// Byte offset of the first character of the placeholder.
    pub start: usize,
    /// Byte offset one past the last character of the placeholder.
    pub end: usize,
}

/// Substitution failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParamError {
    /// No value was given for a variable.
    #[error("no value for the variable {0}")]
    Missing(String),
}

fn _is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn _is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

/// Index one past the end of the word starting at `i`.
fn _word_end(b: &[u8], i: usize) -> usize {
    let mut j = i;
    while j < b.len() && _is_word(b[j]) {
        j += 1;
    }
    j
}

/// Skips a quoted string starting at `i` (doubled quotes and backslashes are escapes).
fn _skip_quoted(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == b'\\' && q != b'`' {
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

/// Length of a PostgreSQL dollar-quote tag (`$$` or `$tag$`) starting at `i`, if there is one.
fn _dollar_tag_len(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let end = _word_end(b, i + 1);
    (b.get(end) == Some(&b'$')).then_some(end + 1 - i)
}

/// Skips a dollar-quoted body starting at `i` (the tag included).
fn _skip_dollar_body(b: &[u8], i: usize, tag_len: usize) -> usize {
    let tag = &b[i..i + tag_len];
    let mut k = i + tag_len;
    while k + tag_len <= b.len() {
        if &b[k..k + tag_len] == tag {
            return k + tag_len;
        }
        k += 1;
    }
    b.len()
}

/// Reads a `{name}` style body starting right after the opening token; returns (name, end).
fn _braced(sql: &str, from: usize, close: &str) -> Option<(String, usize)> {
    let rest = &sql[from..];
    let line_end = rest.find('\n').unwrap_or(rest.len());
    let pos = rest[..line_end].find(close)?;
    let name = rest[..pos].trim();
    (!name.is_empty() && !name.contains('{')).then(|| (name.to_owned(), from + pos + close.len()))
}

/// Finds the placeholder starting at `i`, if the format has one there.
fn _placeholder_at(sql: &str, i: usize, opts: ParamOptions, question_count: &mut usize) -> Option<Placeholder> {
    let b = sql.as_bytes();
    let prev = if i == 0 { None } else { Some(b[i - 1]) };
    match opts.format {
        ParamFormat::Colon => {
            if b[i] != b':' || prev.is_some_and(|p| p == b':' || _is_word(p)) || !b.get(i + 1).copied().is_some_and(_is_word_start) {
                return None;
            }
            let end = _word_end(b, i + 1);
            Some(Placeholder { name: sql[i + 1..end].to_owned(), start: i, end })
        }
        ParamFormat::Percent => {
            if b[i] != b'%' || !b.get(i + 1).copied().is_some_and(_is_word_start) {
                return None;
            }
            let end = _word_end(b, i + 1);
            (b.get(end) == Some(&b'%')).then(|| Placeholder { name: sql[i + 1..end].to_owned(), start: i, end: end + 1 })
        }
        ParamFormat::Question => {
            if b[i] != b'?' {
                return None;
            }
            *question_count += 1;
            Some(Placeholder { name: format!("?{question_count}"), start: i, end: i + 1 })
        }
        ParamFormat::DollarBraces => {
            if !sql[i..].starts_with("${") {
                return None;
            }
            _braced(sql, i + 2, "}").map(|(name, end)| Placeholder { name, start: i, end })
        }
        ParamFormat::Dollar => {
            if b[i] != b'$' || !b.get(i + 1).copied().is_some_and(_is_word_start) {
                return None;
            }
            let end = _word_end(b, i + 1);
            Some(Placeholder { name: sql[i + 1..end].to_owned(), start: i, end })
        }
        ParamFormat::DoubleBraces => {
            if !sql[i..].starts_with("{{") {
                return None;
            }
            _braced(sql, i + 2, "}}").map(|(name, end)| Placeholder { name, start: i, end })
        }
    }
}

/// Finds every placeholder of `sql`, in order of appearance.
pub fn find_placeholders(sql: &str, opts: ParamOptions) -> Vec<Placeholder> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut questions = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p);
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
                continue;
            }
            b'\'' | b'"' | b'`' if !opts.allow_in_quotes => {
                i = _skip_quoted(b, i);
                continue;
            }
            b'$' if !opts.allow_in_quotes => {
                if let Some(len) = _dollar_tag_len(b, i) {
                    i = _skip_dollar_body(b, i, len);
                    continue;
                }
            }
            _ => {}
        }
        if let Some(p) = _placeholder_at(sql, i, opts, &mut questions) {
            i = p.end;
            out.push(p);
            continue;
        }
        // advance one full character so slicing stays on char boundaries
        i += sql[i..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// Distinct variable names of `sql`, in order of first appearance.
pub fn param_names(sql: &str, opts: ParamOptions) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for p in find_placeholders(sql, opts) {
        if !names.contains(&p.name) {
            names.push(p.name);
        }
    }
    names
}

/// Replaces every placeholder with its value, inserted literally.
pub fn substitute(sql: &str, opts: ParamOptions, values: &BTreeMap<String, String>) -> Result<String, ParamError> {
    let mut out = String::with_capacity(sql.len());
    let mut last = 0usize;
    for p in find_placeholders(sql, opts) {
        let Some(v) = values.get(&p.name) else { return Err(ParamError::Missing(p.name)) };
        out.push_str(&sql[last..p.start]);
        out.push_str(v);
        last = p.end;
    }
    out.push_str(&sql[last..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(format: ParamFormat) -> ParamOptions {
        ParamOptions { format, allow_in_quotes: false }
    }

    fn names(sql: &str, format: ParamFormat) -> Vec<String> {
        param_names(sql, opts(format))
    }

    #[test]
    fn colon_finds_names_in_order_without_duplicates() {
        assert_eq!(names("select * from t where a = :id and b = :name or c = :id", ParamFormat::Colon), ["id", "name"]);
    }

    #[test]
    fn colon_ignores_casts_and_assignments() {
        assert!(names("select a::int, b::text from t", ParamFormat::Colon).is_empty());
        assert!(names("select @a := 1", ParamFormat::Colon).is_empty());
        assert!(names("select 10:30", ParamFormat::Colon).is_empty());
    }

    #[test]
    fn quotes_and_comments_are_ignored_by_default() {
        let sql = "select ':a', \":b\", `:c` -- :d\n /* :e */ , :f";
        assert_eq!(names(sql, ParamFormat::Colon), ["f"]);
    }

    #[test]
    fn quoted_placeholders_are_found_when_allowed() {
        let o = ParamOptions { format: ParamFormat::Colon, allow_in_quotes: true };
        assert_eq!(param_names("select ':a' -- :d\n, :f", o), ["a", "f"]);
    }

    #[test]
    fn escaped_quotes_do_not_end_the_string_early() {
        assert_eq!(names("select 'it''s :x', 'a\\' :y', :z", ParamFormat::Colon), ["z"]);
    }

    #[test]
    fn percent_format() {
        assert_eq!(names("select * from t where a = %id% and b like '%x%' and 5 % 2 = 1", ParamFormat::Percent), ["id"]);
    }

    #[test]
    fn question_mark_is_positional() {
        assert_eq!(names("select * from t where a = ? and b = ? and c = '?'", ParamFormat::Question), ["?1", "?2"]);
    }

    #[test]
    fn dollar_braces_format() {
        assert_eq!(names("select ${a}, ${ b }, ${a}", ParamFormat::DollarBraces), ["a", "b"]);
        assert!(names("select ${unclosed", ParamFormat::DollarBraces).is_empty());
    }

    #[test]
    fn dollar_format_skips_numbered_params_and_dollar_quotes() {
        assert_eq!(names("select $a, $b where x = $1", ParamFormat::Dollar), ["a", "b"]);
        assert_eq!(names("do $$ begin select $hidden; end $$; select $shown", ParamFormat::Dollar), ["shown"]);
        assert_eq!(names("do $fn$ select $hidden $fn$; select $shown", ParamFormat::Dollar), ["shown"]);
    }

    #[test]
    fn double_braces_format() {
        assert_eq!(names("select {{a}}, {{ b }} from {{a}}", ParamFormat::DoubleBraces), ["a", "b"]);
    }

    #[test]
    fn substitute_inserts_values_literally() {
        let mut v = BTreeMap::new();
        v.insert("id".to_owned(), "42".to_owned());
        v.insert("name".to_owned(), "'bob'".to_owned());
        let out = substitute("select * from t where id = :id and name = :name and x = :id", opts(ParamFormat::Colon), &v).unwrap();
        assert_eq!(out, "select * from t where id = 42 and name = 'bob' and x = 42");
    }

    #[test]
    fn substitute_positional() {
        let mut v = BTreeMap::new();
        v.insert("?1".to_owned(), "1".to_owned());
        v.insert("?2".to_owned(), "2".to_owned());
        assert_eq!(substitute("select ?, ?", opts(ParamFormat::Question), &v).unwrap(), "select 1, 2");
    }

    #[test]
    fn substitute_reports_missing_values() {
        let err = substitute("select :a", opts(ParamFormat::Colon), &BTreeMap::new()).unwrap_err();
        assert_eq!(err, ParamError::Missing("a".into()));
    }

    #[test]
    fn substitute_keeps_non_ascii_text_intact() {
        let mut v = BTreeMap::new();
        v.insert("a".to_owned(), "1".to_owned());
        assert_eq!(substitute("select 'è' as é, :a", opts(ParamFormat::Colon), &v).unwrap(), "select 'è' as é, 1");
    }

    #[test]
    fn text_without_placeholders_is_unchanged() {
        let sql = "select 1";
        assert_eq!(substitute(sql, opts(ParamFormat::Colon), &BTreeMap::new()).unwrap(), sql);
    }
}
