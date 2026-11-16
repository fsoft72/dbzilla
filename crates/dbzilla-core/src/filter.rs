//! Filter model and the pure `to_where` SQL builder (FR-F-001).

use crate::caps::Capabilities;
use crate::dialect::Dialect;
use crate::model::ColumnMeta;
use crate::value::Value;
use serde::{Deserialize, Serialize};

/// Escape character used in generated `LIKE ... ESCAPE` clauses (valid in every dialect).
const LIKE_ESCAPE: char = '!';
/// Type name fragments of numeric columns, whose values are emitted unquoted.
const NUMERIC_TYPES: [&str; 9] = ["int", "serial", "numeric", "decimal", "float", "double", "real", "number", "money"];
/// Type name fragments of columns that cannot be matched as text.
const BINARY_TYPES: [&str; 4] = ["blob", "bytea", "binary", "geometry"];

/// Comparison operator of a filter row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterOp {
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    In,
    NotIn,
    IsNull,
    IsNotNull,
    Between,
    NotBetween,
    Like,
    Contains,
    NotContains,
    HasPrefix,
    HasSuffix,
    ILike,
    IContains,
    INotContains,
    IHasPrefix,
    IHasSuffix,
}

impl FilterOp {
    /// Text shown in the operator menu.
    pub fn label(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "<>",
            FilterOp::Lt => "<",
            FilterOp::Gt => ">",
            FilterOp::Le => "<=",
            FilterOp::Ge => ">=",
            FilterOp::In => "IN",
            FilterOp::NotIn => "NOT IN",
            FilterOp::IsNull => "IS NULL",
            FilterOp::IsNotNull => "IS NOT NULL",
            FilterOp::Between => "BETWEEN",
            FilterOp::NotBetween => "NOT BETWEEN",
            FilterOp::Like => "LIKE",
            FilterOp::Contains => "Contains",
            FilterOp::NotContains => "Not contains",
            FilterOp::HasPrefix => "Has prefix",
            FilterOp::HasSuffix => "Has suffix",
            FilterOp::ILike => "ILIKE",
            FilterOp::IContains => "Contains (i)",
            FilterOp::INotContains => "Not contains (i)",
            FilterOp::IHasPrefix => "Has prefix (i)",
            FilterOp::IHasSuffix => "Has suffix (i)",
        }
    }

    /// False for operators that take no value.
    pub fn needs_value(self) -> bool {
        !matches!(self, FilterOp::IsNull | FilterOp::IsNotNull)
    }

    /// Operators offered by a dialect, in menu order.
    pub fn available(caps: &Capabilities) -> Vec<FilterOp> {
        use FilterOp::*;
        let mut ops = vec![
            Eq, Ne, Lt, Gt, Le, Ge, In, NotIn, IsNull, IsNotNull, Between, NotBetween, Like, Contains, NotContains,
            HasPrefix, HasSuffix,
        ];
        if caps.ilike {
            ops.extend([ILike, IContains, INotContains, IHasPrefix, IHasSuffix]);
        }
        ops
    }
}

/// Which column(s) a filter applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterColumn {
    Column(String),
    Any,
    Raw,
}

/// One filter row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filter {
    pub enabled: bool,
    pub column: FilterColumn,
    pub op: FilterOp,
    pub value: String,
}

/// Builds the `WHERE` condition (without the keyword) from the enabled filters.
/// Incomplete filters (for example `BETWEEN` without `AND`) are skipped. Returns an empty string
/// when nothing applies.
pub fn to_where(d: &dyn Dialect, cols: &[ColumnMeta], filters: &[Filter], or: bool) -> String {
    let mut parts: Vec<String> = filters.iter().filter(|f| f.enabled).filter_map(|f| filter_sql(d, cols, f)).collect();
    if parts.len() <= 1 {
        return parts.pop().unwrap_or_default();
    }
    let joiner = if or { " OR " } else { " AND " };
    parts.iter().map(|p| format!("({p})")).collect::<Vec<_>>().join(joiner)
}

/// SQL for one filter row.
fn filter_sql(d: &dyn Dialect, cols: &[ColumnMeta], f: &Filter) -> Option<String> {
    match &f.column {
        FilterColumn::Raw => {
            let raw = f.value.trim();
            if raw.is_empty() {
                return None;
            }
            // a raw filter is one expression: more than one statement could change data, so it matches nothing
            if crate::splitter::split_statements(raw, crate::dialect::DriverId::MySql).len() > 1 {
                return Some("1 = 0".to_owned());
            }
            Some(raw.to_owned())
        }
        FilterColumn::Column(name) => {
            let meta = cols.iter().find(|c| &c.name == name);
            op_sql(d, &d.quote_ident(name), false, meta, f.op, &f.value)
        }
        FilterColumn::Any => {
            let parts: Vec<String> = cols
                .iter()
                .filter(|c| is_text_castable(c))
                .filter_map(|c| op_sql(d, &d.cast_text(&d.quote_ident(&c.name)), true, None, f.op, &f.value))
                .collect();
            if parts.is_empty() { Some("1 = 0".to_owned()) } else { Some(format!("({})", parts.join(" OR "))) }
        }
    }
}

/// True when the column can take part in an "any column" text search.
fn is_text_castable(c: &ColumnMeta) -> bool {
    let t = c.type_name.to_ascii_lowercase();
    !BINARY_TYPES.iter().any(|b| t.contains(b))
}

/// True when the column is numeric and values should be emitted unquoted.
fn is_numeric(meta: Option<&ColumnMeta>) -> bool {
    meta.is_some_and(|m| {
        let t = m.type_name.to_ascii_lowercase();
        !t.contains("point") && NUMERIC_TYPES.iter().any(|n| t.contains(n))
    })
}

/// True for plain finite numbers (no `inf` / `nan`).
fn is_number(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        && s.parse::<f64>().is_ok_and(f64::is_finite)
}

/// Renders a user-typed scalar as a literal, unquoted for numbers on numeric columns.
fn scalar(d: &dyn Dialect, meta: Option<&ColumnMeta>, raw: &str) -> String {
    if is_numeric(meta) && is_number(raw.trim()) {
        return raw.trim().to_owned();
    }
    d.literal(&Value::Text(raw.to_owned()))
}

/// Escapes `LIKE` wildcards with [`LIKE_ESCAPE`].
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == LIKE_ESCAPE || c == '%' || c == '_' {
            out.push(LIKE_ESCAPE);
        }
        out.push(c);
    }
    out
}

/// Splits a comma separated list, honouring single quotes; quoted items lose their quotes.
fn split_list(value: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut in_quote = false;
    let mut chars = value.chars().peekable();
    let flush = |cur: &mut String, quoted: &mut bool, items: &mut Vec<String>| {
        let item = if *quoted { cur.clone() } else { cur.trim().to_owned() };
        if *quoted || !item.is_empty() {
            items.push(item);
        }
        cur.clear();
        *quoted = false;
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' if in_quote && chars.peek() == Some(&'\'') => {
                cur.push('\'');
                chars.next();
            }
            '\'' => {
                if !quoted && cur.trim().is_empty() {
                    cur.clear();
                }
                in_quote = !in_quote;
                quoted = true;
            }
            ',' if !in_quote => flush(&mut cur, &mut quoted, &mut items),
            c if quoted && !in_quote && c.is_whitespace() => {}
            _ => cur.push(c),
        }
    }
    flush(&mut cur, &mut quoted, &mut items);
    items
}

/// Splits `a AND b` (case-insensitive) into its two trimmed parts.
fn split_between(value: &str) -> Option<(String, String)> {
    let idx = value.to_ascii_lowercase().find(" and ")?;
    let (a, b) = (value[..idx].trim(), value[idx + 5..].trim());
    if a.is_empty() || b.is_empty() { None } else { Some((a.to_owned(), b.to_owned())) }
}

/// Builds a `LIKE` / `ILIKE` comparison.
fn like_sql(d: &dyn Dialect, text_expr: &str, negate: bool, insensitive: bool, pattern: &str, escape: bool) -> String {
    let lit = d.literal(&Value::Text(pattern.to_owned()));
    let not = if negate { "NOT " } else { "" };
    let esc = if escape { format!(" ESCAPE '{LIKE_ESCAPE}'") } else { String::new() };
    if insensitive && Capabilities::for_driver(d.id()).ilike {
        return format!("{text_expr} {not}ILIKE {lit}{esc}");
    }
    if insensitive {
        return format!("LOWER({text_expr}) {not}LIKE LOWER({lit}){esc}");
    }
    format!("{text_expr} {not}LIKE {lit}{esc}")
}

/// SQL for one operator applied to `expr`.
fn op_sql(d: &dyn Dialect, expr: &str, is_text: bool, meta: Option<&ColumnMeta>, op: FilterOp, value: &str) -> Option<String> {
    use FilterOp::*;
    let text_expr = if is_text { expr.to_owned() } else { d.text_operand(expr) };
    let cmp = |sym: &str| Some(format!("{expr} {sym} {}", scalar(d, meta, value)));
    match op {
        Eq => cmp("="),
        Ne => cmp("<>"),
        Lt => cmp("<"),
        Gt => cmp(">"),
        Le => cmp("<="),
        Ge => cmp(">="),
        In | NotIn => {
            let items = split_list(value);
            if items.is_empty() {
                return Some(if op == In { "1 = 0" } else { "1 = 1" }.to_owned());
            }
            let list: Vec<String> = items.iter().map(|i| scalar(d, meta, i)).collect();
            Some(format!("{expr} {}IN ({})", if op == NotIn { "NOT " } else { "" }, list.join(", ")))
        }
        IsNull => Some(format!("{expr} IS NULL")),
        IsNotNull => Some(format!("{expr} IS NOT NULL")),
        Between | NotBetween => {
            let (a, b) = split_between(value)?;
            Some(format!(
                "{expr} {}BETWEEN {} AND {}",
                if op == NotBetween { "NOT " } else { "" },
                scalar(d, meta, &a),
                scalar(d, meta, &b)
            ))
        }
        Like => Some(like_sql(d, &text_expr, false, false, value, false)),
        ILike => Some(like_sql(d, &text_expr, false, true, value, false)),
        Contains => Some(like_sql(d, &text_expr, false, false, &format!("%{}%", escape_like(value)), true)),
        NotContains => Some(like_sql(d, &text_expr, true, false, &format!("%{}%", escape_like(value)), true)),
        HasPrefix => Some(like_sql(d, &text_expr, false, false, &format!("{}%", escape_like(value)), true)),
        HasSuffix => Some(like_sql(d, &text_expr, false, false, &format!("%{}", escape_like(value)), true)),
        IContains => Some(like_sql(d, &text_expr, false, true, &format!("%{}%", escape_like(value)), true)),
        INotContains => Some(like_sql(d, &text_expr, true, true, &format!("%{}%", escape_like(value)), true)),
        IHasPrefix => Some(like_sql(d, &text_expr, false, true, &format!("{}%", escape_like(value)), true)),
        IHasSuffix => Some(like_sql(d, &text_expr, false, true, &format!("%{}", escape_like(value)), true)),
    }
}
