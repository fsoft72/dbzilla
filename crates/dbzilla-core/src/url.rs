//! Connection URL parsing and formatting (SPEC 6.3).

use crate::dialect::DriverId;
use percent_encoding::{percent_decode_str, utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use std::collections::BTreeMap;
use thiserror::Error;
use url::Url;

/// Characters kept as-is when encoding URL components.
const ENCODE: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');
/// Same as [`ENCODE`] but keeps path separators.
const ENCODE_PATH: &AsciiSet = &ENCODE.remove(b'/');
/// Default PostgreSQL port.
const PG_PORT: u16 = 5432;
/// Default MySQL port.
const MYSQL_PORT: u16 = 3306;

/// Errors from URL handling.
#[derive(Debug, Error, PartialEq)]
pub enum UrlError {
    #[error("the connection URL scheme '{0}' is not supported by this version")]
    UnknownScheme(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("invalid connection URL: {0}")]
    Invalid(String),
}

/// Table to open right after connecting (URL `schema`, `table`, `column`, ... options).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OpenTable {
    pub schema: Option<String>,
    pub table: String,
    pub column: Option<String>,
    pub operation: Option<String>,
    pub value: Option<String>,
    /// Raw SQL `WHERE` fragment used instead of column/operation/value.
    pub condition: Option<String>,
}

/// Everything a connection URL can carry.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionSpec {
    pub driver: DriverId,
    pub name: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub password: Option<String>,
    /// Database name, or file path for SQLite.
    pub database: String,
    pub status_color: Option<String>,
    pub env: Option<String>,
    pub safe_level: Option<u8>,
    pub tls_mode: Option<u8>,
    pub open_table: Option<OpenTable>,
    /// Options this version does not interpret, preserved for round trips.
    pub extra: BTreeMap<String, String>,
}

impl ConnectionSpec {
    /// An empty spec for a driver.
    pub fn new(driver: DriverId) -> Self {
        Self {
            driver,
            name: None,
            host: String::new(),
            port: None,
            user: String::new(),
            password: None,
            database: String::new(),
            status_color: None,
            env: None,
            safe_level: None,
            tls_mode: None,
            open_table: None,
            extra: BTreeMap::new(),
        }
    }
}

/// Default TCP port of a driver, if it has one.
pub fn default_port(d: DriverId) -> Option<u16> {
    match d {
        DriverId::Postgres => Some(PG_PORT),
        DriverId::MySql => Some(MYSQL_PORT),
        DriverId::Sqlite => None,
    }
}

/// Parses a numeric option value.
fn parse_u8(key: &str, v: &str) -> Result<u8, UrlError> {
    v.parse().map_err(|_| UrlError::Invalid(format!("option {key} must be a small number, got '{v}'")))
}

/// Parses a connection URL.
pub fn parse_url(input: &str) -> Result<ConnectionSpec, UrlError> {
    let url = Url::parse(input.trim()).map_err(|e| UrlError::Invalid(e.to_string()))?;
    let scheme = url.scheme().to_owned();
    if scheme.contains("+ssh") {
        return Err(UrlError::Unsupported("SSH tunnel URLs are not supported yet".into()));
    }
    let driver = match scheme.as_str() {
        "postgresql" | "postgres" => DriverId::Postgres,
        "mysql" | "mariadb" => DriverId::MySql,
        "sqlite" => DriverId::Sqlite,
        _ => return Err(UrlError::UnknownScheme(scheme)),
    };
    let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
    let mut spec = ConnectionSpec::new(driver);
    spec.host = url.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']').to_owned();
    spec.port = url.port();
    spec.user = decode(url.username());
    spec.password = url.password().map(decode);
    let path = decode(url.path());
    spec.database = if driver == DriverId::Sqlite { path } else { path.trim_start_matches('/').to_owned() };
    let mut open = OpenTable::default();
    for (k, v) in url.query_pairs() {
        let v = v.into_owned();
        match k.as_ref() {
            "name" => spec.name = Some(v),
            "statusColor" => spec.status_color = Some(v),
            "env" => spec.env = Some(v),
            "safeModeLevel" => spec.safe_level = Some(parse_u8("safeModeLevel", &v)?),
            "tLSMode" => spec.tls_mode = Some(parse_u8("tLSMode", &v)?),
            "schema" => open.schema = Some(v),
            "table" | "view" | "item" => open.table = v,
            "column" => open.column = Some(v),
            "operation" | "operator" => open.operation = Some(v),
            "value" => open.value = Some(v),
            "condition" | "raw" | "query" => open.condition = Some(v),
            other => {
                spec.extra.insert(other.to_owned(), v);
            }
        }
    }
    if !open.table.is_empty() {
        spec.open_table = Some(open);
    }
    Ok(spec)
}

/// Formats a spec as a URL, omitting the default port and optionally the password.
pub fn format_url(spec: &ConnectionSpec, include_password: bool) -> String {
    let enc = |s: &str| utf8_percent_encode(s, ENCODE).to_string();
    let query = query_pairs(spec).into_iter().map(|(k, v)| format!("{k}={}", enc(&v))).collect::<Vec<_>>().join("&");
    let query = if query.is_empty() { String::new() } else { format!("?{query}") };
    if spec.driver == DriverId::Sqlite {
        return format!("sqlite://{}{query}", utf8_percent_encode(&spec.database, ENCODE_PATH));
    }
    let scheme = if spec.driver == DriverId::Postgres { "postgresql" } else { "mysql" };
    let password = if include_password { spec.password.as_deref() } else { None };
    let userinfo = match (spec.user.is_empty(), password) {
        (true, None) => String::new(),
        (_, None) => format!("{}@", enc(&spec.user)),
        (_, Some(pw)) => format!("{}:{}@", enc(&spec.user), enc(pw)),
    };
    let host = if spec.host.contains(':') { format!("[{}]", spec.host) } else { spec.host.clone() };
    let port = match spec.port {
        Some(p) if Some(p) != default_port(spec.driver) => format!(":{p}"),
        _ => String::new(),
    };
    format!("{scheme}://{userinfo}{host}{port}/{}{query}", enc(&spec.database))
}

/// Ordered query options of a spec.
fn query_pairs(spec: &ConnectionSpec) -> Vec<(String, String)> {
    let mut q: Vec<(String, String)> = Vec::new();
    let mut add = |k: &str, v: Option<String>| {
        if let Some(v) = v {
            q.push((k.to_owned(), v));
        }
    };
    add("name", spec.name.clone());
    add("statusColor", spec.status_color.clone());
    add("env", spec.env.clone());
    add("tLSMode", spec.tls_mode.map(|v| v.to_string()));
    add("safeModeLevel", spec.safe_level.map(|v| v.to_string()));
    for (k, v) in &spec.extra {
        add(k, Some(v.clone()));
    }
    if let Some(t) = &spec.open_table {
        add("schema", t.schema.clone());
        add("table", Some(t.table.clone()));
        add("column", t.column.clone());
        add("operation", t.operation.clone());
        add("value", t.value.clone());
        add("condition", t.condition.clone());
    }
    q
}
