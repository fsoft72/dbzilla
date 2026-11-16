//! Streaming export to CSV, JSON, SQL (optionally gzip) and XLSX (SPEC 12.2).

use crate::naming::{render_name, NameContext};
use crate::{IoError, Progress, ProgressFn, BATCH_ROWS};
use dbzilla_core::dialect::{dialect_for, Dialect, DriverId};
use dbzilla_core::model::{ColumnMeta, Row, TableRef};
use dbzilla_core::value::Value;
use dbzilla_drivers::{Conn, QueryOpts, RowBatch};
use flate2::write::GzEncoder;
use flate2::Compression;
use rust_xlsxwriter::{Workbook, Worksheet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Maximum rows of an Excel sheet.
pub const XLSX_MAX_ROWS: u64 = 1_048_576;

/// Output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
    Sql,
    Xlsx,
}

impl ExportFormat {
    /// File extension.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Json => "json",
            ExportFormat::Sql => "sql",
            ExportFormat::Xlsx => "xlsx",
        }
    }
}

/// Line terminator of CSV files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineBreak {
    Lf,
    CrLf,
    Cr,
}

/// CSV options (SPEC 12.2).
#[derive(Debug, Clone, PartialEq)]
pub struct CsvExportOptions {
    pub delimiter: char,
    pub quote: char,
    pub quote_always: bool,
    pub line_break: LineBreak,
    pub decimal: char,
    pub null_as_empty: bool,
    pub header: bool,
    pub newline_to_space: bool,
}

impl Default for CsvExportOptions {
    /// Comma, double quote when needed, LF, dot decimal, NULL written as `NULL`, header row.
    fn default() -> Self {
        Self { delimiter: ',', quote: '"', quote_always: false, line_break: LineBreak::Lf, decimal: '.', null_as_empty: false, header: true, newline_to_space: false }
    }
}

/// SQL export options: structure, drop statement, data.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlExportOptions {
    pub structure: bool,
    pub drop: bool,
    pub data: bool,
}

impl Default for SqlExportOptions {
    fn default() -> Self {
        Self { structure: true, drop: false, data: true }
    }
}

/// Everything about the output.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub csv: CsvExportOptions,
    pub sql: SqlExportOptions,
    /// Compress SQL dumps with gzip.
    pub gzip: bool,
    pub dir: PathBuf,
    pub name_pattern: String,
}

/// What to export.
#[derive(Debug, Clone)]
pub enum ExportSource {
    Table { table: TableRef, columns: Option<Vec<String>>, where_sql: Option<String>, order_sql: Option<String>, page: Option<(u64, u64)> },
    Query { name: String, sql: String },
}

impl ExportSource {
    /// Display and file name.
    fn name(&self) -> String {
        match self {
            ExportSource::Table { table, .. } => table.name.clone(),
            ExportSource::Query { name, .. } => name.clone(),
        }
    }
}

/// The SELECT of a table source.
fn select_sql(d: &dyn Dialect, src: &ExportSource) -> String {
    match src {
        ExportSource::Query { sql, .. } => sql.trim().trim_end_matches(';').to_owned(),
        ExportSource::Table { table, columns, where_sql, order_sql, page } => {
            let cols = columns.as_ref().filter(|c| !c.is_empty()).map_or("*".to_owned(), |c| c.iter().map(|x| d.quote_ident(x)).collect::<Vec<_>>().join(", "));
            let mut sql = format!("SELECT {cols} FROM {}", d.quote_table(table));
            if let Some(w) = where_sql.as_ref().filter(|w| !w.trim().is_empty()) {
                sql.push_str(&format!(" WHERE {w}"));
            }
            if let Some(o) = order_sql.as_ref().filter(|o| !o.trim().is_empty()) {
                sql.push_str(&format!(" ORDER BY {o}"));
            }
            if let Some((offset, limit)) = page {
                sql.push(' ');
                sql.push_str(&d.limit_offset(*limit, *offset));
            }
            sql
        }
    }
}

/// A destination of rows.
trait RowWriter: Send {
    fn begin(&mut self, cols: &[ColumnMeta]) -> Result<(), IoError>;
    fn rows(&mut self, rows: &[Row]) -> Result<(), IoError>;
    fn finish(self: Box<Self>) -> Result<(), IoError>;
}

/// Text of a cell for CSV.
fn csv_cell(v: &Value, o: &CsvExportOptions) -> String {
    let text = match v {
        Value::Null if o.null_as_empty => String::new(),
        Value::Null => "NULL".to_owned(),
        Value::Bytes(b) => format!("0x{}", b.iter().map(|x| format!("{x:02X}")).collect::<String>()),
        Value::F64(_) | Value::Decimal(_) if o.decimal != '.' => v.to_display().replace('.', &o.decimal.to_string()),
        other => other.to_display(),
    };
    if o.newline_to_space { text.replace("\r\n", " ").replace(['\n', '\r'], " ") } else { text }
}

/// CSV writer.
struct CsvRows {
    w: csv::Writer<Box<dyn Write + Send>>,
    o: CsvExportOptions,
}

impl RowWriter for CsvRows {
    fn begin(&mut self, cols: &[ColumnMeta]) -> Result<(), IoError> {
        if self.o.header {
            self.w.write_record(cols.iter().map(|c| c.name.as_str())).map_err(|e| IoError::Format(e.to_string()))?;
        }
        Ok(())
    }

    fn rows(&mut self, rows: &[Row]) -> Result<(), IoError> {
        for r in rows {
            self.w.write_record(r.iter().map(|v| csv_cell(v, &self.o))).map_err(|e| IoError::Format(e.to_string()))?;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), IoError> {
        self.w.flush()?;
        Ok(())
    }
}

/// JSON array of objects.
struct JsonRows {
    out: Box<dyn Write + Send>,
    names: Vec<String>,
    first: bool,
}

/// JSON value of a cell.
pub fn json_value(v: &Value) -> serde_json::Value {
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

impl RowWriter for JsonRows {
    fn begin(&mut self, cols: &[ColumnMeta]) -> Result<(), IoError> {
        self.names = cols.iter().map(|c| c.name.clone()).collect();
        self.out.write_all(b"[")?;
        Ok(())
    }

    fn rows(&mut self, rows: &[Row]) -> Result<(), IoError> {
        for r in rows {
            let obj: serde_json::Map<String, serde_json::Value> = self.names.iter().zip(r).map(|(n, v)| (n.clone(), json_value(v))).collect();
            self.out.write_all(if self.first { b"\n  " } else { b",\n  " })?;
            self.first = false;
            serde_json::to_writer(&mut self.out, &serde_json::Value::Object(obj)).map_err(|e| IoError::Format(e.to_string()))?;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), IoError> {
        self.out.write_all(if self.first { b"]\n" } else { b"\n]\n" })?;
        self.out.flush()?;
        Ok(())
    }
}

/// SQL INSERT dump of one table (the file is shared by all tables).
struct SqlRows<'a> {
    out: &'a mut (dyn Write + Send),
    d: Box<dyn Dialect>,
    table: TableRef,
    cols: Vec<String>,
    pending: Vec<String>,
    data: bool,
}

impl SqlRows<'_> {
    /// Writes the buffered rows as one multi-row INSERT.
    fn flush(&mut self) -> Result<(), IoError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let cols = self.cols.iter().map(|c| self.d.quote_ident(c)).collect::<Vec<_>>().join(", ");
        writeln!(self.out, "INSERT INTO {} ({cols}) VALUES\n  {};", self.d.quote_table(&self.table), self.pending.join(",\n  "))?;
        self.pending.clear();
        Ok(())
    }
}

impl RowWriter for SqlRows<'_> {
    fn begin(&mut self, cols: &[ColumnMeta]) -> Result<(), IoError> {
        self.cols = cols.iter().map(|c| c.name.clone()).collect();
        Ok(())
    }

    fn rows(&mut self, rows: &[Row]) -> Result<(), IoError> {
        if !self.data {
            return Ok(());
        }
        for r in rows {
            self.pending.push(format!("({})", r.iter().map(|v| self.d.literal(v)).collect::<Vec<_>>().join(", ")));
            if self.pending.len() >= BATCH_ROWS {
                self.flush()?;
            }
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), IoError> {
        self.flush()?;
        writeln!(self.out)?;
        Ok(())
    }
}

/// XLSX workbook with one sheet.
struct XlsxRows {
    book: Workbook,
    path: PathBuf,
    row: u32,
    header: bool,
}

impl XlsxRows {
    /// The single worksheet.
    fn sheet(&mut self) -> Result<&mut Worksheet, IoError> {
        self.book.worksheet_from_index(0).map_err(|e| IoError::Format(e.to_string()))
    }
}

impl RowWriter for XlsxRows {
    fn begin(&mut self, cols: &[ColumnMeta]) -> Result<(), IoError> {
        if self.header {
            let names: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
            let sheet = self.sheet()?;
            for (i, n) in names.iter().enumerate() {
                sheet.write_string(0, i as u16, n).map_err(|e| IoError::Format(e.to_string()))?;
            }
            self.row = 1;
        }
        Ok(())
    }

    fn rows(&mut self, rows: &[Row]) -> Result<(), IoError> {
        for r in rows {
            if u64::from(self.row) >= XLSX_MAX_ROWS {
                return Err(IoError::Format(format!("Excel sheets hold at most {XLSX_MAX_ROWS} rows; use CSV for larger tables")));
            }
            let row = self.row;
            let sheet = self.sheet()?;
            for (i, v) in r.iter().enumerate() {
                let col = i as u16;
                let res = match v {
                    Value::Null => Ok(&mut *sheet),
                    Value::Bool(b) => sheet.write_boolean(row, col, *b),
                    Value::I64(n) => sheet.write_number(row, col, *n as f64),
                    Value::U64(n) => sheet.write_number(row, col, *n as f64),
                    Value::F64(f) => sheet.write_number(row, col, *f),
                    other => sheet.write_string(row, col, other.to_display()),
                };
                res.map_err(|e| IoError::Format(e.to_string()))?;
            }
            self.row += 1;
        }
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<(), IoError> {
        let path = self.path.clone();
        self.book.save(path).map_err(|e| IoError::Format(e.to_string()))
    }
}

/// Opens an output file, gzip-wrapped when asked.
fn create_out(path: &Path, gzip: bool) -> Result<Box<dyn Write + Send>, IoError> {
    let f = BufWriter::new(File::create(path)?);
    Ok(if gzip { Box::new(GzEncoder::new(f, Compression::default())) } else { Box::new(f) })
}

/// Runs the query of one source into `writer`, honouring cancel and reporting progress.
#[allow(clippy::too_many_arguments)]
async fn stream_into(conn: &mut dyn Conn, sql: &str, writer: &mut dyn RowWriter, name: &str, done: usize, total: usize, progress: ProgressFn<'_>, cancel: &AtomicBool) -> Result<u64, IoError> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<RowBatch>(4);
    let consumer = async {
        let mut count = 0u64;
        while let Some(batch) = rx.recv().await {
            if cancel.load(Ordering::SeqCst) {
                return Err(IoError::Cancelled);
            }
            if let Some(cols) = &batch.columns {
                writer.begin(cols)?;
            }
            writer.rows(&batch.rows)?;
            count += batch.rows.len() as u64;
            progress(Progress { item: name.to_owned(), rows: count, done_items: done, total_items: total });
        }
        Ok(count)
    };
    let query = conn.query_stream(sql, QueryOpts::default(), tx);
    let (q, c) = tokio::join!(query, consumer);
    let count = c?;
    q?;
    Ok(count)
}

/// Exports the sources; returns the files written. A cancelled or failed export leaves no partial file.
pub async fn run_export(conn: &mut dyn Conn, driver: DriverId, sources: &[ExportSource], opts: &ExportOptions, ctx: &NameContext, progress: ProgressFn<'_>, cancel: &AtomicBool) -> Result<Vec<PathBuf>, IoError> {
    std::fs::create_dir_all(&opts.dir)?;
    let d = dialect_for(driver);
    if opts.format == ExportFormat::Sql {
        return export_sql(conn, driver, sources, opts, ctx, progress, cancel).await;
    }
    let mut files = Vec::new();
    for (i, src) in sources.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err(IoError::Cancelled);
        }
        let name = src.name();
        let path = opts.dir.join(format!("{}.{}", render_name(&opts.name_pattern, &name, ctx), opts.format.extension()));
        let sql = select_sql(d.as_ref(), src);
        let mut writer: Box<dyn RowWriter> = match opts.format {
            ExportFormat::Csv => {
                let o = opts.csv.clone();
                let (delim, quote) = (u8::try_from(o.delimiter).map_err(|_| IoError::Format("the CSV delimiter must be an ASCII character".into()))?, u8::try_from(o.quote).map_err(|_| IoError::Format("the CSV quote must be an ASCII character".into()))?);
                let terminator = match o.line_break {
                    LineBreak::Lf => csv::Terminator::Any(b'\n'),
                    LineBreak::CrLf => csv::Terminator::CRLF,
                    LineBreak::Cr => csv::Terminator::Any(b'\r'),
                };
                let w = csv::WriterBuilder::new()
                    .delimiter(delim)
                    .quote(quote)
                    .quote_style(if o.quote_always { csv::QuoteStyle::Always } else { csv::QuoteStyle::Necessary })
                    .terminator(terminator)
                    .has_headers(false)
                    .from_writer(create_out(&path, false)?);
                Box::new(CsvRows { w, o })
            }
            ExportFormat::Json => Box::new(JsonRows { out: create_out(&path, false)?, names: Vec::new(), first: true }),
            ExportFormat::Xlsx => {
                let mut book = Workbook::new();
                book.add_worksheet();
                Box::new(XlsxRows { book, path: path.clone(), row: 0, header: opts.csv.header })
            }
            ExportFormat::Sql => unreachable!("handled above"),
        };
        match stream_into(conn, &sql, writer.as_mut(), &name, i, sources.len(), progress, cancel).await {
            Ok(_) => {
                writer.finish()?;
                files.push(path);
            }
            Err(e) => {
                drop(writer);
                let _ = std::fs::remove_file(&path);
                return Err(e);
            }
        }
    }
    Ok(files)
}

/// One SQL dump file for all sources.
async fn export_sql(conn: &mut dyn Conn, driver: DriverId, sources: &[ExportSource], opts: &ExportOptions, ctx: &NameContext, progress: ProgressFn<'_>, cancel: &AtomicBool) -> Result<Vec<PathBuf>, IoError> {
    let d = dialect_for(driver);
    let base = if sources.len() == 1 { sources[0].name() } else { "export".to_owned() };
    let ext = if opts.gzip { "sql.gz" } else { "sql" };
    let path = opts.dir.join(format!("{}.{ext}", render_name(&opts.name_pattern, &base, ctx)));
    let mut out = create_out(&path, opts.gzip)?;
    let result = async {
        writeln!(out, "-- DBZilla SQL export {} {}", ctx.date, ctx.time)?;
        for (i, src) in sources.iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                return Err(IoError::Cancelled);
            }
            let ExportSource::Table { table, .. } = src else { return Err(IoError::Unsupported("query results cannot be exported as SQL".into())) };
            writeln!(out, "\n-- Table: {}", table.name)?;
            if opts.sql.drop {
                writeln!(out, "DROP TABLE IF EXISTS {};", d.quote_table(table))?;
            }
            if opts.sql.structure {
                let st = conn.table_structure(table).await?;
                let ddl = st.ddl.trim().trim_end_matches(';').to_owned();
                writeln!(out, "{ddl};")?;
            }
            let mut rows = SqlRows { out: &mut *out, d: dialect_for(driver), table: table.clone(), cols: Vec::new(), pending: Vec::new(), data: opts.sql.data };
            let sql = select_sql(d.as_ref(), src);
            let mut writer: Box<dyn RowWriter + Send + '_> = Box::new(SqlRowsRef(&mut rows));
            stream_into(conn, &sql, writer.as_mut(), &table.name, i, sources.len(), progress, cancel).await?;
            writer.finish()?;
        }
        Ok::<(), IoError>(())
    }
    .await;
    match result {
        Ok(()) => {
            out.flush()?;
            drop(out);
            Ok(vec![path])
        }
        Err(e) => {
            drop(out);
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    }
}

/// Adapter so a borrowed `SqlRows` can be used as a boxed writer.
struct SqlRowsRef<'a, 'b>(&'a mut SqlRows<'b>);

impl RowWriter for SqlRowsRef<'_, '_> {
    fn begin(&mut self, cols: &[ColumnMeta]) -> Result<(), IoError> {
        self.0.begin(cols)
    }
    fn rows(&mut self, rows: &[Row]) -> Result<(), IoError> {
        self.0.rows(rows)
    }
    fn finish(self: Box<Self>) -> Result<(), IoError> {
        self.0.flush()?;
        writeln!(self.0.out)?;
        Ok(())
    }
}
