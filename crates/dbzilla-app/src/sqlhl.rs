//! SQL tokenizer and syntax highlighting for the editor (SPEC 10.3).

use crate::theme;
use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{Color32, FontId};
use std::collections::HashSet;

/// Kind of a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    Keyword,
    Ident,
    QuotedIdent,
    String,
    Number,
    Comment,
    Punct,
    Space,
}

/// A token as a byte range of the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: Tok,
    pub start: usize,
    pub end: usize,
}

/// Extra words highlighted as keywords on top of the dialect list.
const EXTRA_KEYWORDS: [&str; 24] = [
    "IF", "REPLACE", "RETURNING", "OVER", "PARTITION", "WINDOW", "ROWS", "RANGE", "UNBOUNDED", "PRECEDING", "FOLLOWING", "CURRENT", "ROW", "NULLS", "FIRST", "LAST", "FILTER", "COLLATE", "IGNORE", "USING", "NATURAL", "EXCEPT",
    "INTERSECT", "RECURSIVE",
];

/// End of a quoted region starting at `i` with quote `q` (doubled quotes escape).
fn quoted_end(b: &[u8], i: usize, q: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() {
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

/// End of a PostgreSQL dollar quote starting at `i`, if one starts there.
fn dollar_end(sql: &str, i: usize) -> Option<usize> {
    let b = sql.as_bytes();
    let mut j = i + 1;
    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
        j += 1;
    }
    if b.get(j) != Some(&b'$') || b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let tag = &sql[i..=j];
    Some(sql[j + 1..].find(tag).map_or(b.len(), |p| j + 1 + p + tag.len()))
}

/// Splits the text into tokens covering it completely.
pub fn tokenize(sql: &str, keywords: &HashSet<String>) -> Vec<Token> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let kind = match b[i] {
            c if c.is_ascii_whitespace() => {
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                Tok::Space
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p);
                Tok::Comment
            }
            b'#' => {
                i = b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p);
                Tok::Comment
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
                Tok::Comment
            }
            b'\'' => {
                i = quoted_end(b, i, b'\'');
                Tok::String
            }
            q @ (b'"' | b'`') => {
                i = quoted_end(b, i, q);
                Tok::QuotedIdent
            }
            b'$' => match dollar_end(sql, i) {
                Some(end) => {
                    i = end;
                    Tok::String
                }
                None => {
                    i += 1;
                    Tok::Punct
                }
            },
            c if c.is_ascii_digit() => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                    i += 1;
                }
                Tok::Number
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80) {
                    i += 1;
                }
                if keywords.contains(&sql[start..i].to_ascii_uppercase()) { Tok::Keyword } else { Tok::Ident }
            }
            _ => {
                i += sql[i..].chars().next().map_or(1, char::len_utf8);
                Tok::Punct
            }
        };
        out.push(Token { kind, start, end: i });
    }
    out
}

/// Keyword set for a dialect.
pub fn keyword_set(dialect_keywords: &[&str]) -> HashSet<String> {
    dialect_keywords.iter().chain(EXTRA_KEYWORDS.iter()).map(|k| (*k).to_owned()).collect()
}

/// Colour of a token kind.
fn color_of(k: Tok) -> Color32 {
    match k {
        Tok::Keyword => theme::SECONDARY,
        Tok::String => theme::OK,
        Tok::Number => theme::TERTIARY,
        Tok::Comment => theme::MUTED,
        Tok::QuotedIdent | Tok::Ident | Tok::Punct | Tok::Space => theme::TEXT,
    }
}

/// Builds the layout job of a text (the editor sets the wrap width per frame).
pub fn layout_job(text: &str, keywords: &HashSet<String>, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob { text: text.to_owned(), ..Default::default() };
    for t in tokenize(text, keywords) {
        let format = TextFormat { font_id: font.clone(), color: color_of(t.kind), italics: t.kind == Tok::Comment, ..Default::default() };
        job.sections.push(egui::text::LayoutSection { leading_space: 0.0, byte_range: t.start..t.end, format });
    }
    job
}

use eframe::egui;

#[cfg(test)]
mod tests {
    use super::*;

    fn kw() -> HashSet<String> {
        keyword_set(&["SELECT", "FROM", "WHERE"])
    }

    fn kinds(sql: &str) -> Vec<(Tok, &str)> {
        tokenize(sql, &kw()).into_iter().filter(|t| t.kind != Tok::Space).map(|t| (t.kind, &sql[t.start..t.end])).collect()
    }

    /// Keywords, identifiers, strings, numbers and comments are told apart.
    #[test]
    fn token_kinds() {
        assert_eq!(
            kinds("select a, 'x''y' from \"T\" where n >= 1.5e3 -- hi"),
            vec![
                (Tok::Keyword, "select"),
                (Tok::Ident, "a"),
                (Tok::Punct, ","),
                (Tok::String, "'x''y'"),
                (Tok::Keyword, "from"),
                (Tok::QuotedIdent, "\"T\""),
                (Tok::Keyword, "where"),
                (Tok::Ident, "n"),
                (Tok::Punct, ">"),
                (Tok::Punct, "="),
                (Tok::Number, "1.5e3"),
                (Tok::Comment, "-- hi"),
            ]
        );
    }

    /// Block comments, dollar quotes and unterminated input.
    #[test]
    fn special_regions() {
        assert_eq!(kinds("/* a */ select"), vec![(Tok::Comment, "/* a */"), (Tok::Keyword, "select")]);
        assert_eq!(kinds("select $$ x ; y $$"), vec![(Tok::Keyword, "select"), (Tok::String, "$$ x ; y $$")]);
        assert_eq!(kinds("select $1"), vec![(Tok::Keyword, "select"), (Tok::Punct, "$"), (Tok::Number, "1")]);
        assert_eq!(kinds("select 'open"), vec![(Tok::Keyword, "select"), (Tok::String, "'open")]);
        assert_eq!(kinds("/* open"), vec![(Tok::Comment, "/* open")]);
    }

    /// Tokens always cover the whole text, even with unicode.
    #[test]
    fn tokens_cover_the_text() {
        for sql in ["select 'è' as città, 日本 from t; -- ✓", "", "   ", "a\nb\r\nc", "x$y"] {
            let toks = tokenize(sql, &kw());
            let joined: String = toks.iter().map(|t| &sql[t.start..t.end]).collect();
            assert_eq!(joined, sql);
        }
    }

    /// The layout job keeps the text and colours keywords with the accent.
    #[test]
    fn layout_job_sections() {
        let job = layout_job("select 1", &kw(), &FontId::monospace(13.0));
        assert_eq!(job.text, "select 1");
        assert_eq!(job.sections[0].format.color, theme::SECONDARY);
        assert_eq!(job.sections.last().unwrap().format.color, theme::TERTIARY);
    }
}
