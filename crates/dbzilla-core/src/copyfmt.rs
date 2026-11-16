//! Copy / export text formats and "Copy Script As" (SPEC 8.5, 8.10).

use crate::ddl::{create_from_structure, generate_ddl, StructuralOp};
use crate::dialect::Dialect;
use crate::model::{ColumnMeta, Row, TableRef, TableStructure};
use crate::value::Value;

/// Text representation of a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFormat {
    PlainText,
    Json,
    Html,
    Markdown,
    Csv,
    CsvWithHeader,
    InsertStatement,
}

/// CSV settings (SPEC 14.1 defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct CsvOptions {
    pub delimiter: char,
    pub quote: char,
    pub quote_always: bool,
    pub line_break: String,
    pub decimal: char,
    pub null_as_empty: bool,
}

impl Default for CsvOptions {
    /// Comma, double quote when needed, LF, dot decimal, NULL written as `NULL`.
    fn default() -> Self {
        Self { delimiter: ',', quote: '"', quote_always: false, line_break: "\n".into(), decimal: '.', null_as_empty: false }
    }
}

/// Context for formatting.
pub struct CopyContext<'a> {
    pub dialect: &'a dyn Dialect,
    pub table: Option<&'a TableRef>,
    pub csv: &'a CsvOptions,
}

/// Which script "Copy Script As" produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Creation,
    Drop,
    Truncate,
    SelectTop100,
}

/// Text of one cell for text formats.
fn cell_text(v: &Value, o: &CsvOptions) -> String {
    match v {
        Value::Null if o.null_as_empty => String::new(),
        Value::Null => "NULL".to_owned(),
        Value::F64(_) | Value::Decimal(_) if o.decimal != '.' => v.to_display().replace('.', &o.decimal.to_string()),
        other => other.to_display(),
    }
}

/// Quotes a CSV field when needed (or always).
fn csv_field(s: &str, o: &CsvOptions) -> String {
    let needs = o.quote_always || s.contains(o.delimiter) || s.contains(o.quote) || s.contains('\n') || s.contains('\r');
    if !needs {
        return s.to_owned();
    }
    let q = o.quote.to_string();
    format!("{q}{}{q}", s.replace(&q, &format!("{q}{q}")))
}

/// JSON value of a cell.
fn json_value(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => (*b).into(),
        Value::I64(n) => (*n).into(),
        Value::U64(n) => (*n).into(),
        Value::F64(f) => serde_json::Number::from_f64(*f).map_or(serde_json::Value::Null, Into::into),
        Value::Json(j) => j.clone(),
        Value::Bytes(b) => format!("0x{}", b.iter().map(|x| format!("{x:02X}")).collect::<String>()).into(),
        other => other.to_display().into(),
    }
}

/// HTML-escapes text.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Formats rows in the requested text format.
pub fn format_rows(fmt: CopyFormat, cols: &[ColumnMeta], rows: &[Row], ctx: &CopyContext) -> String {
    let names: Vec<&str> = cols.iter().map(|c| c.name.as_str()).collect();
    let plain = CsvOptions::default();
    match fmt {
        CopyFormat::PlainText => rows.iter().map(|r| r.iter().map(|v| cell_text(v, &plain)).collect::<Vec<_>>().join("\t")).collect::<Vec<_>>().join("\n"),
        CopyFormat::Csv | CopyFormat::CsvWithHeader => {
            let o = ctx.csv;
            let delim = o.delimiter.to_string();
            let mut lines: Vec<String> = Vec::new();
            if fmt == CopyFormat::CsvWithHeader {
                lines.push(names.iter().map(|n| csv_field(n, o)).collect::<Vec<_>>().join(&delim));
            }
            for r in rows {
                lines.push(r.iter().map(|v| csv_field(&cell_text(v, o), o)).collect::<Vec<_>>().join(&delim));
            }
            lines.join(&o.line_break)
        }
        CopyFormat::Json => {
            let objs: Vec<serde_json::Value> = rows
                .iter()
                .map(|r| {
                    let map: serde_json::Map<String, serde_json::Value> = names.iter().zip(r).map(|(n, v)| ((*n).to_owned(), json_value(v))).collect();
                    serde_json::Value::Object(map)
                })
                .collect();
            serde_json::to_string_pretty(&objs).unwrap_or_default()
        }
        CopyFormat::Markdown => {
            let esc = |s: String| s.replace('|', "\\|").replace('\n', "<br>");
            let mut lines = vec![format!("| {} |", names.join(" | ")), format!("| {} |", names.iter().map(|_| "---").collect::<Vec<_>>().join(" | "))];
            for r in rows {
                lines.push(format!("| {} |", r.iter().map(|v| esc(cell_text(v, &plain))).collect::<Vec<_>>().join(" | ")));
            }
            lines.join("\n")
        }
        CopyFormat::Html => {
            let head: String = names.iter().map(|n| format!("<th>{}</th>", html_escape(n))).collect();
            let body: String = rows
                .iter()
                .map(|r| format!("<tr>{}</tr>", r.iter().map(|v| format!("<td>{}</td>", html_escape(&cell_text(v, &plain)))).collect::<String>()))
                .collect();
            format!("<table><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>")
        }
        CopyFormat::InsertStatement => {
            let d = ctx.dialect;
            let table = ctx.table.map_or_else(|| d.quote_ident("table_name"), |t| d.quote_table(t));
            let col_list = names.iter().map(|n| d.quote_ident(n)).collect::<Vec<_>>().join(", ");
            rows.iter()
                .map(|r| format!("INSERT INTO {table} ({col_list}) VALUES ({});", r.iter().map(|v| d.literal(v)).collect::<Vec<_>>().join(", ")))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
}

/// Builds the "Copy Script As" text for a table.
pub fn copy_script(kind: ScriptKind, st: &TableStructure, d: &dyn Dialect) -> String {
    let table = &st.table;
    let one = |op: StructuralOp| generate_ddl(&op, d).ok().and_then(|v| v.into_iter().next()).unwrap_or_default();
    match kind {
        ScriptKind::Creation => {
            let ddl = if st.ddl.trim().is_empty() { create_from_structure(st, d) } else { st.ddl.trim().to_owned() };
            if ddl.ends_with(';') { ddl } else { format!("{ddl};") }
        }
        ScriptKind::Drop => format!("{};", one(StructuralOp::DropTable { table: table.clone(), cascade: false })),
        ScriptKind::Truncate => format!("{};", one(StructuralOp::Truncate { table: table.clone(), cascade: false, restart_identity: false })),
        ScriptKind::SelectTop100 => format!("SELECT * FROM {} {};", d.quote_table(table), d.limit_offset(100, 0).replace(" OFFSET 0", "")),
    }
}
