//! Context aware SQL autocompletion (SPEC 10.5): tables, columns, keywords and saved queries.

use crate::fuzzy::{match_item, Strategy};
use crate::sqlhl::{tokenize, Tok};
use crate::sqltext::statement_at;
use dbzilla_core::dialect::DriverId;
use std::collections::{HashMap, HashSet};

/// Maximum number of suggestions returned.
pub const MAX_SUGGESTIONS: usize = 12;
/// Minimum prefix length before suggestions appear on their own.
pub const MIN_PREFIX: usize = 2;
/// Clause keywords after which tables are expected.
const TABLE_CLAUSES: [&str; 6] = ["FROM", "JOIN", "INTO", "UPDATE", "TABLE", "TRUNCATE"];
/// Clause keywords after which columns are expected.
const COLUMN_CLAUSES: [&str; 8] = ["SELECT", "WHERE", "ON", "SET", "GROUP", "ORDER", "HAVING", "BY"];
/// Words that end a table reference (they cannot be an alias).
const NOT_AN_ALIAS: [&str; 17] = ["ON", "WHERE", "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "GROUP", "ORDER", "HAVING", "LIMIT", "UNION", "USING", "NATURAL", "SET"];

/// What a suggestion is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Table,
    View,
    Column,
    Keyword,
    Snippet,
}

/// One entry of the completion list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub kind: Kind,
    pub detail: String,
    pub insert: String,
}

/// What is expected at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    Table,
    Column { qualifier: Option<String> },
    Keyword,
}

/// Parsed completion context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// Byte offset where the typed word starts.
    pub start: usize,
    pub prefix: String,
    pub expect: Expect,
    /// `(table, alias)` pairs found in the statement.
    pub tables: Vec<(String, Option<String>)>,
}

/// Columns `(name, type)` of a table, when known.
pub type ColumnLookup<'a> = &'a dyn Fn(&str) -> Option<Vec<(String, String)>>;

/// Schema knowledge the engine uses.
pub struct Schema<'a> {
    /// `(name, is_view)`.
    pub tables: Vec<(&'a str, bool)>,
    /// Columns `(name, type)` of a table when known.
    pub columns: ColumnLookup<'a>,
    pub keywords: &'a [&'a str],
    /// Keyword to SQL text of saved queries.
    pub saved: &'a HashMap<String, String>,
}

/// True for characters of an identifier.
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Start of the word ending at `cursor` and the word itself.
pub fn word_before(text: &str, cursor: usize) -> (usize, String) {
    let mut cursor = cursor.min(text.len());
    while !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let start = text[..cursor].char_indices().rev().take_while(|(_, c)| is_ident(*c)).last().map_or(cursor, |(i, _)| i);
    (start, text[start..cursor].to_owned())
}

/// True when the cursor is inside a string or a comment.
fn in_string_or_comment(text: &str, cursor: usize) -> bool {
    let kw = HashSet::new();
    tokenize(text, &kw).iter().any(|t| matches!(t.kind, Tok::String | Tok::Comment) && t.start < cursor && cursor <= t.end && (cursor < t.end || t.kind == Tok::Comment && !text[t.start..t.end].contains('\n') || !text[t.start..t.end].ends_with(['\'', '$'])))
}

/// Tables referenced by FROM / JOIN / UPDATE / INTO in a statement.
pub fn tables_in(stmt: &str) -> Vec<(String, Option<String>)> {
    let kw = HashSet::new();
    let toks: Vec<(Tok, String)> = tokenize(stmt, &kw).into_iter().filter(|t| !matches!(t.kind, Tok::Space | Tok::Comment)).map(|t| (t.kind, stmt[t.start..t.end].to_owned())).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let upper = toks[i].1.to_ascii_uppercase();
        if matches!(upper.as_str(), "FROM" | "JOIN" | "UPDATE" | "INTO") {
            let mut j = i + 1;
            while let Some((_, name)) = toks.get(j).filter(|(k, _)| matches!(k, Tok::Ident | Tok::QuotedIdent)) {
                // table name, possibly schema qualified
                let mut table = unquote(name);
                j += 1;
                while toks.get(j).is_some_and(|(_, t)| t == ".") && toks.get(j + 1).is_some_and(|(k, _)| matches!(k, Tok::Ident | Tok::QuotedIdent)) {
                    table = unquote(&toks[j + 1].1);
                    j += 2;
                }
                if toks.get(j).is_some_and(|(_, t)| t.eq_ignore_ascii_case("AS")) {
                    j += 1;
                }
                let alias = toks.get(j).filter(|(k, t)| matches!(k, Tok::Ident | Tok::QuotedIdent) && !NOT_AN_ALIAS.contains(&t.to_ascii_uppercase().as_str())).map(|(_, t)| unquote(t));
                if alias.is_some() {
                    j += 1;
                }
                out.push((table, alias));
                if toks.get(j).is_some_and(|(_, t)| t == ",") {
                    j += 1;
                    continue;
                }
                break;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Removes identifier quotes.
fn unquote(s: &str) -> String {
    s.trim_matches(|c| c == '"' || c == '`').to_owned()
}

/// Analyses the text before the cursor; `None` inside strings and comments.
pub fn context(text: &str, cursor: usize, dialect: DriverId) -> Option<Context> {
    let cursor = cursor.min(text.len());
    if !text.is_char_boundary(cursor) || in_string_or_comment(text, cursor) {
        return None;
    }
    let (start, prefix) = word_before(text, cursor);
    let stmt = statement_at(text, cursor, dialect);
    let (stmt_text, stmt_start) = stmt.as_ref().map_or(("", 0), |s| (s.text.as_str(), s.start));
    let tables = tables_in(stmt_text);
    // qualifier: `alias.` right before the word
    let before = &text[..start];
    if let Some(rest) = before.strip_suffix('.') {
        let (_, q) = word_before(rest, rest.len());
        if !q.is_empty() {
            return Some(Context { start, prefix, expect: Expect::Column { qualifier: Some(q) }, tables });
        }
    }
    // clause keyword before the cursor inside this statement
    let upto = &text[stmt_start.min(start)..start];
    let kw = HashSet::new();
    let clause = tokenize(upto, &kw)
        .into_iter()
        .rev()
        .filter(|t| t.kind == Tok::Ident)
        .map(|t| upto[t.start..t.end].to_ascii_uppercase())
        .find(|w| TABLE_CLAUSES.contains(&w.as_str()) || COLUMN_CLAUSES.contains(&w.as_str()));
    let expect = match clause.as_deref() {
        Some(c) if TABLE_CLAUSES.contains(&c) && !after_table_name(upto) => Expect::Table,
        Some(c) if COLUMN_CLAUSES.contains(&c) => Expect::Column { qualifier: None },
        _ => Expect::Keyword,
    };
    Some(Context { start, prefix, expect, tables })
}

/// True when the text before the cursor already holds a table name after FROM/JOIN (so keywords are next).
fn after_table_name(upto: &str) -> bool {
    let kw = HashSet::new();
    let toks: Vec<String> = tokenize(upto, &kw).into_iter().filter(|t| t.kind != Tok::Space).map(|t| upto[t.start..t.end].to_ascii_uppercase()).collect();
    match toks.as_slice() {
        [.., a, b] if TABLE_CLAUSES.contains(&a.as_str()) && b != "," && !TABLE_CLAUSES.contains(&b.as_str()) => true,
        [.., a, _, c] if TABLE_CLAUSES.contains(&a.as_str()) && c != "," && c != "." => true,
        _ => false,
    }
}

/// Builds the suggestions for a context.
pub fn suggest(ctx: &Context, schema: &Schema, explicit: bool, uppercase_keywords: bool, quote: &dyn Fn(&str) -> String) -> Vec<Suggestion> {
    let has_qualifier = matches!(&ctx.expect, Expect::Column { qualifier: Some(_) });
    if ctx.prefix.chars().count() < MIN_PREFIX && !has_qualifier && !explicit {
        return Vec::new();
    }
    let mut scored: Vec<(i32, Suggestion)> = Vec::new();
    let mut push = |score: i32, s: Suggestion| scored.push((score, s));
    match &ctx.expect {
        Expect::Table => {
            for (name, view) in &schema.tables {
                if let Some(s) = match_item(&ctx.prefix, name, Strategy::Fuzzy) {
                    push(s + 50, Suggestion { label: (*name).to_owned(), kind: if *view { Kind::View } else { Kind::Table }, detail: if *view { "view" } else { "table" }.into(), insert: quote(name) });
                }
            }
        }
        Expect::Column { qualifier } => {
            let wanted: Vec<&str> = match qualifier {
                Some(q) => ctx.tables.iter().filter(|(t, a)| a.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q)) || (a.is_none() && t.eq_ignore_ascii_case(q))).map(|(t, _)| t.as_str()).collect(),
                None => ctx.tables.iter().map(|(t, _)| t.as_str()).collect(),
            };
            let wanted = if wanted.is_empty() && qualifier.is_some() { vec![qualifier.as_deref().unwrap_or_default()] } else { wanted };
            for t in wanted {
                for (col, ty) in (schema.columns)(t).unwrap_or_default() {
                    if let Some(s) = match_item(&ctx.prefix, &col, Strategy::Fuzzy) {
                        push(s + 60, Suggestion { label: col.clone(), kind: Kind::Column, detail: format!("{t}  {ty}"), insert: quote(&col) });
                    }
                }
            }
            if qualifier.is_none() {
                for (name, view) in &schema.tables {
                    if let Some(s) = match_item(&ctx.prefix, name, Strategy::Fuzzy) {
                        push(s, Suggestion { label: (*name).to_owned(), kind: if *view { Kind::View } else { Kind::Table }, detail: "table".into(), insert: quote(name) });
                    }
                }
            }
        }
        Expect::Keyword => {}
    }
    if !has_qualifier {
        for k in schema.keywords {
            if let Some(s) = match_item(&ctx.prefix, k, Strategy::Fuzzy) {
                let text = if uppercase_keywords { (*k).to_owned() } else { k.to_lowercase() };
                push(s + if matches!(ctx.expect, Expect::Keyword) { 20 } else { 0 }, Suggestion { label: text.clone(), kind: Kind::Keyword, detail: "keyword".into(), insert: text });
            }
        }
        for (k, sql) in schema.saved {
            if let Some(s) = match_item(&ctx.prefix, k, Strategy::Fuzzy) {
                push(s + 80, Suggestion { label: k.clone(), kind: Kind::Snippet, detail: "saved query".into(), insert: sql.clone() });
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.to_lowercase().cmp(&b.1.label.to_lowercase())));
    let mut seen = HashSet::new();
    let list: Vec<Suggestion> = scored.into_iter().map(|(_, s)| s).filter(|s| seen.insert((s.label.to_lowercase(), s.kind as u8))).take(MAX_SUGGESTIONS).collect();
    if list.len() == 1 && list[0].kind != Kind::Snippet && list[0].label.eq_ignore_ascii_case(&ctx.prefix) {
        return Vec::new();
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(t: &str) -> Option<Vec<(String, String)>> {
        match t {
            "orders" => Some(vec![("id".into(), "integer".into()), ("customer_id".into(), "integer".into()), ("status".into(), "text".into())]),
            "customers" => Some(vec![("id".into(), "integer".into()), ("name".into(), "text".into())]),
            _ => None,
        }
    }

    fn run(sql_with_bar: &str, explicit: bool) -> Vec<String> {
        let cursor = sql_with_bar.find('|').expect("cursor marker");
        let text = sql_with_bar.replace('|', "");
        let Some(ctx) = context(&text, cursor, DriverId::Sqlite) else { return vec![] };
        let saved: HashMap<String, String> = [("rev".to_owned(), "SELECT sum(total) FROM orders".to_owned())].into_iter().collect();
        let schema = Schema { tables: vec![("orders", false), ("customers", false), ("v_open", true)], columns: &cols, keywords: &["SELECT", "FROM", "WHERE", "ORDER", "GROUP"], saved: &saved };
        suggest(&ctx, &schema, explicit, true, &|s| s.to_owned()).into_iter().map(|s| s.label).collect()
    }

    /// After FROM the tables are suggested.
    #[test]
    fn tables_after_from() {
        assert_eq!(run("SELECT * FROM or|", false).first().map(String::as_str), Some("orders"));
        assert!(run("SELECT * FROM v_|", false).contains(&"v_open".to_owned()));
        assert!(run("SELECT * FROM orders o JOIN cu|", false).contains(&"customers".to_owned()));
    }

    /// An alias qualifier completes the columns of its table.
    #[test]
    fn alias_columns() {
        let got = run("SELECT o.| FROM orders o", false);
        assert_eq!(got, vec!["customer_id", "id", "status"]);
        assert_eq!(run("SELECT * FROM orders o WHERE o.st|", false).first().map(String::as_str), Some("status"));
        assert_eq!(run("SELECT * FROM orders o JOIN customers c ON c.|", false), vec!["id", "name"]);
    }

    /// Columns of every table in the statement are offered, including tables after the cursor.
    #[test]
    fn columns_from_statement_tables() {
        let got = run("SELECT na| FROM customers c", false);
        assert_eq!(got.first().map(String::as_str), Some("name"));
        let got = run("SELECT * FROM orders WHERE cus|", false);
        assert_eq!(got.first().map(String::as_str), Some("customer_id"));
    }

    /// Keywords complete and follow the upper-case option; saved queries appear as snippets.
    #[test]
    fn keywords_and_saved_queries() {
        assert!(run("sel|", false).contains(&"SELECT".to_owned()));
        let lower = {
            let text = "sel";
            let ctx = context(text, 3, DriverId::Sqlite).unwrap();
            let saved = HashMap::new();
            let schema = Schema { tables: vec![], columns: &cols, keywords: &["SELECT"], saved: &saved };
            suggest(&ctx, &schema, false, false, &|s| s.to_owned()).into_iter().map(|s| s.insert).collect::<Vec<_>>()
        };
        assert_eq!(lower, vec!["select"]);
        assert_eq!(run("rev|", false).first().map(String::as_str), Some("rev"));
    }

    /// Nothing is suggested for short prefixes unless invoked explicitly, nor inside strings and comments.
    #[test]
    fn restraint() {
        assert!(run("s|", false).is_empty());
        assert!(!run("s|", true).is_empty());
        assert!(run("SELECT 'or|'", false).is_empty());
        assert!(run("SELECT 1 -- or|", false).is_empty());
        assert!(run("SELECT * FROM orders|", false).is_empty(), "an exact single match needs no popup");
    }

    /// Table references with schemas, aliases, AS and comma lists are found.
    #[test]
    fn table_parsing() {
        assert_eq!(tables_in("SELECT * FROM public.orders AS o, customers c JOIN v_open ON 1=1 WHERE x"), vec![("orders".into(), Some("o".into())), ("customers".into(), Some("c".into())), ("v_open".into(), None)]);
        assert_eq!(tables_in("UPDATE orders SET a = 1"), vec![("orders".into(), None)]);
        assert_eq!(tables_in("SELECT 1"), vec![]);
        assert_eq!(tables_in("SELECT * FROM \"My Table\" t"), vec![("My Table".into(), Some("t".into()))]);
    }

    /// The word before the cursor is found on unicode text.
    #[test]
    fn word_before_cursor() {
        let text = "select città";
        assert_eq!(word_before(text, text.len()), (7, "città".into()));
        assert_eq!(word_before(text, text.len() - 1), (7, "citt".into()), "a cursor inside a character moves back");
        assert_eq!(word_before("a. ", 3), (3, String::new()));
    }
}
