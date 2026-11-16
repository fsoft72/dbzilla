//! Rewrites a SQLite `CREATE TABLE` text so one column changes while every other constraint stays as written.

/// Kind of a scanned token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Word,
    Quoted,
    Str,
    Group,
    Punct,
}

/// A token with its original text.
#[derive(Debug, Clone)]
struct Tok {
    kind: Kind,
    raw: String,
}

/// End offset (exclusive) of a quoted run starting at `i`; `close` ends it, a doubled `close` is an escape.
fn quoted_end(b: &[u8], i: usize, close: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        if b[j] == close {
            if b.get(j + 1) == Some(&close) && close != b']' {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += 1;
    }
    b.len()
}

/// End offset (exclusive) of a parenthesised group starting at `i`.
fn group_end(b: &[u8], i: usize) -> usize {
    let (mut depth, mut j) = (0usize, i);
    while j < b.len() {
        match b[j] {
            b'\'' => j = quoted_end(b, j, b'\''),
            b'"' => j = quoted_end(b, j, b'"'),
            b'`' => j = quoted_end(b, j, b'`'),
            b'(' => {
                depth += 1;
                j += 1;
            }
            b')' => {
                depth -= 1;
                j += 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => j += 1,
        }
    }
    b.len()
}

/// Splits SQL into tokens, skipping blanks and comments.
fn scan(sql: &str) -> Vec<Tok> {
    let b = sql.as_bytes();
    let (mut i, mut out) = (0, Vec::new());
    while i < b.len() {
        let c = b[i];
        let (kind, end) = match c {
            _ if c.is_ascii_whitespace() => {
                i += 1;
                continue;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i = b[i..].iter().position(|&x| x == b'\n').map_or(b.len(), |p| i + p);
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
                continue;
            }
            b'\'' => (Kind::Str, quoted_end(b, i, b'\'')),
            b'"' => (Kind::Quoted, quoted_end(b, i, b'"')),
            b'`' => (Kind::Quoted, quoted_end(b, i, b'`')),
            b'[' => (Kind::Quoted, quoted_end(b, i, b']')),
            b'(' => (Kind::Group, group_end(b, i)),
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c == b'.' || c >= 0x80 => {
                let mut j = i;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || matches!(b[j], b'_' | b'$' | b'.') || b[j] >= 0x80) {
                    j += 1;
                }
                (Kind::Word, j)
            }
            _ => (Kind::Punct, i + 1),
        };
        let end = end.min(b.len()).max(i + 1);
        out.push(Tok { kind, raw: sql[i..end].to_owned() });
        i = end;
    }
    out
}

/// Identifier text without its quotes.
fn unquote(t: &Tok) -> String {
    let r = t.raw.as_str();
    match t.kind {
        Kind::Quoted if r.len() >= 2 => {
            let (open, inner) = (r.as_bytes()[0], &r[1..r.len() - 1]);
            match open {
                b'"' => inner.replace("\"\"", "\""),
                b'`' => inner.replace("``", "`"),
                _ => inner.to_owned(),
            }
        }
        _ => r.to_owned(),
    }
}

/// True when `sql` uses `name` as an identifier anywhere, including inside groups.
pub fn mentions_ident(sql: &str, name: &str) -> bool {
    scan(sql).iter().any(|t| match t.kind {
        Kind::Word | Kind::Quoted => unquote(t).eq_ignore_ascii_case(name),
        Kind::Group => mentions_ident(&t.raw[1..t.raw.len().saturating_sub(1).max(1)], name),
        _ => false,
    })
}

/// Words that start a column constraint, so the type ends before them.
const CONSTRAINT_WORDS: [&str; 11] = ["CONSTRAINT", "PRIMARY", "NOT", "NULL", "UNIQUE", "CHECK", "DEFAULT", "COLLATE", "REFERENCES", "GENERATED", "AS"];
/// Words that start a table constraint.
const TABLE_WORDS: [&str; 5] = ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"];

/// True for a word token equal to `w` (case insensitive).
fn is_word(t: &Tok, w: &str) -> bool {
    t.kind == Kind::Word && t.raw.eq_ignore_ascii_case(w)
}

/// Column constraints without NOT NULL, NULL and DEFAULT (the caller adds the new ones).
fn clean_rest(rest: &[Tok]) -> Result<String, String> {
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let t = &rest[i];
        if is_word(t, "CONSTRAINT") {
            return Err("a named column constraint".into());
        }
        if is_word(t, "GENERATED") || is_word(t, "AS") {
            return Err("a generated column".into());
        }
        if is_word(t, "NOT") && rest.get(i + 1).is_some_and(|n| is_word(n, "NULL")) {
            i += 2;
            continue;
        }
        if is_word(t, "NULL") {
            i += 1;
            continue;
        }
        if is_word(t, "DEFAULT") {
            i += 1;
            if rest.get(i).is_some_and(|n| n.kind == Kind::Punct && (n.raw == "+" || n.raw == "-")) {
                i += 1;
            }
            i += 1;
            if rest.get(i).is_some_and(|n| n.kind == Kind::Group) && rest.get(i - 1).is_some_and(|p| p.kind == Kind::Word) {
                i += 1; // function call such as datetime('now')
            }
            continue;
        }
        out.push(&t.raw);
        i += 1;
    }
    Ok(out.join(" "))
}

/// What to change in the table text.
pub struct ColumnChange<'a> {
    pub old_name: &'a str,
    pub new_name: &'a str,
    pub new_type: &'a str,
    pub nullable: bool,
    pub default: Option<&'a str>,
}

/// Rewrites `ddl` into a `CREATE TABLE <tmp_quoted> (...)` where only the changed column differs.
/// Errors name what the rewrite cannot keep safe; the caller then refuses the operation.
pub fn rewrite_create(ddl: &str, tmp_quoted: &str, ch: &ColumnChange<'_>, quote: &dyn Fn(&str) -> String) -> Result<String, String> {
    let toks = scan(ddl);
    let open = toks.iter().position(|t| t.kind == Kind::Group).ok_or("the table text has no column list")?;
    let body = &toks[open].raw[1..toks[open].raw.len() - 1];
    let suffix: Vec<&str> = toks[open + 1..].iter().map(|t| t.raw.as_str()).collect();
    let mut elements: Vec<Vec<Tok>> = vec![Vec::new()];
    for t in scan(body) {
        if t.kind == Kind::Punct && t.raw == "," {
            elements.push(Vec::new());
        } else if let Some(last) = elements.last_mut() {
            last.push(t);
        }
    }
    let renamed = !ch.old_name.eq_ignore_ascii_case(ch.new_name);
    let mut rendered = Vec::new();
    let mut found = false;
    for el in &elements {
        let Some(first) = el.first() else { continue };
        let is_table_constraint = first.kind == Kind::Word && TABLE_WORDS.iter().any(|w| first.raw.eq_ignore_ascii_case(w));
        let is_target = !is_table_constraint && matches!(first.kind, Kind::Word | Kind::Quoted) && unquote(first).eq_ignore_ascii_case(ch.old_name);
        if is_target {
            found = true;
            let type_end = el.iter().skip(1).position(|t| t.kind == Kind::Word && CONSTRAINT_WORDS.iter().any(|w| t.raw.eq_ignore_ascii_case(w))).map_or(el.len(), |p| p + 1);
            let rest = &el[type_end..];
            if renamed && rest.iter().any(|t| mentions_ident(&t.raw, ch.old_name)) {
                return Err("a constraint of the column refers to its old name".into());
            }
            let mut text = format!("{} {}", quote(ch.new_name), ch.new_type);
            let kept = clean_rest(rest)?;
            if !kept.is_empty() {
                text.push(' ');
                text.push_str(&kept);
            }
            if !ch.nullable {
                text.push_str(" NOT NULL");
            }
            if let Some(d) = ch.default {
                text.push_str(&format!(" DEFAULT {d}"));
            }
            rendered.push(text);
        } else {
            if !is_table_constraint && el.iter().skip(1).any(|t| is_word(t, "GENERATED") || is_word(t, "AS")) {
                return Err("a generated column cannot be copied by a rebuild".into());
            }
            let raw = el.iter().map(|t| t.raw.as_str()).collect::<Vec<_>>().join(" ");
            if renamed && mentions_ident(&raw, ch.old_name) {
                return Err("another definition refers to the renamed column".into());
            }
            rendered.push(raw);
        }
    }
    if !found {
        return Err("the column was not found in the table text".into());
    }
    let tail = if suffix.is_empty() { String::new() } else { format!(" {}", suffix.join(" ")) };
    Ok(format!("CREATE TABLE {tmp_quoted} (\n  {}\n){tail}", rendered.join(",\n  ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(n: &str) -> String {
        format!("\"{n}\"")
    }

    fn change<'a>(old: &'a str, new: &'a str, ty: &'a str) -> ColumnChange<'a> {
        ColumnChange { old_name: old, new_name: new, new_type: ty, nullable: true, default: None }
    }

    /// Constraints written next to the column and at table level survive.
    #[test]
    fn keeps_unrelated_constraints() {
        let ddl = "CREATE TABLE a (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT UNIQUE COLLATE NOCASE, c INTEGER NOT NULL DEFAULT 5 CHECK (c > 0), UNIQUE (id, email), FOREIGN KEY (id) REFERENCES b(x)) STRICT";
        let out = rewrite_create(ddl, "\"a__new\"", &change("email", "email", "VARCHAR(80)"), &q).unwrap();
        assert!(out.contains("\"email\" VARCHAR(80) UNIQUE COLLATE NOCASE"), "{out}");
        assert!(out.contains("PRIMARY KEY AUTOINCREMENT"), "{out}");
        assert!(out.contains("UNIQUE (id, email)"), "{out}");
        assert!(out.contains("FOREIGN KEY (id) REFERENCES b (x)"), "{out}");
        assert!(out.contains("c INTEGER NOT NULL DEFAULT 5 CHECK (c > 0)"), "the other column keeps its text: {out}");
        assert!(out.ends_with("STRICT"), "{out}");
    }

    /// NOT NULL and DEFAULT come from the new definition, not from the old text.
    #[test]
    fn replaces_nullability_and_default() {
        let ddl = "CREATE TABLE a (id INTEGER, c INTEGER NOT NULL DEFAULT (1 + 2) CHECK (c > 0), d TEXT DEFAULT datetime('now'))";
        let ch = ColumnChange { old_name: "c", new_name: "c", new_type: "TEXT", nullable: true, default: Some("'x'") };
        let out = rewrite_create(ddl, "\"t\"", &ch, &q).unwrap();
        assert!(out.contains("\"c\" TEXT CHECK (c > 0) DEFAULT 'x'"), "{out}");
        assert!(out.contains("d TEXT DEFAULT datetime ('now')"), "{out}");
        let nn = ColumnChange { old_name: "d", new_name: "d", new_type: "TEXT", nullable: false, default: None };
        let out = rewrite_create(ddl, "\"t\"", &nn, &q).unwrap();
        assert!(out.contains("\"d\" TEXT NOT NULL") && !out.contains("datetime"), "{out}");
    }

    /// A rename is refused when something else still names the old column.
    #[test]
    fn refuses_unsafe_renames_and_generated_columns() {
        let table_level = "CREATE TABLE a (id INTEGER, c INTEGER, UNIQUE (c))";
        assert!(rewrite_create(table_level, "t", &change("c", "d", "INTEGER"), &q).is_err());
        assert!(rewrite_create(table_level, "t", &change("c", "c", "TEXT"), &q).is_ok(), "same name: nothing else changes");
        assert!(rewrite_create("CREATE TABLE a (id INTEGER, g INTEGER GENERATED ALWAYS AS (id + 1))", "t", &change("g", "g", "TEXT"), &q).is_err());
        assert!(rewrite_create("CREATE TABLE a (id INTEGER, c INTEGER CONSTRAINT nn NOT NULL)", "t", &change("c", "c", "TEXT"), &q).is_err());
        assert!(rewrite_create("CREATE TABLE a (id INTEGER)", "t", &change("zz", "zz", "TEXT"), &q).is_err());
    }

    /// Identifier lookup sees quoted names and nested groups, not string literals.
    #[test]
    fn identifier_mentions() {
        assert!(mentions_ident("CREATE INDEX i ON a (\"C\")", "c"));
        assert!(mentions_ident("CREATE INDEX i ON a (lower(c))", "c"));
        assert!(!mentions_ident("CREATE INDEX i ON a (x) WHERE y = 'c'", "c"));
    }
}
