//! Command line parsing (SPEC FR-C-021, FR-C-022).

use dbzilla_core::url::{parse_url, ConnectionSpec};
use std::path::PathBuf;

/// File extensions opened directly as SQLite databases.
const SQLITE_EXTENSIONS: [&str; 3] = ["sqlite", "sqlite3", "db"];

/// What the command line asks to open.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenTarget {
    Url(Box<ConnectionSpec>),
    SqliteFile(PathBuf),
}

/// Parsed command line.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CliArgs {
    pub data_dir: Option<PathBuf>,
    pub open: Option<OpenTarget>,
    pub help: bool,
    /// Problems found; the app shows them and carries on.
    pub warnings: Vec<String>,
}

/// Help text.
pub const HELP: &str = "DBZilla - native database client\n\nUSAGE:\n  dbzilla [OPTIONS] [URL | FILE]\n\nOPTIONS:\n  --data-dir <DIR>   store settings and data in DIR\n  -h, --help         show this help\n\nURL examples:\n  postgresql://user@host/db?schema=public&table=orders\n  mysql://root@127.0.0.1/shop\n  sqlserver://sa@127.0.0.1/master\n  sqlite:///path/to/file.sqlite\nFILE: a .sqlite, .sqlite3 or .db file";

/// Parses arguments (without the program name). Unknown flags only produce a warning.
pub fn parse_args(args: &[String]) -> CliArgs {
    let mut out = CliArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => out.help = true,
            "--data-dir" => match it.next() {
                Some(d) => out.data_dir = Some(PathBuf::from(d)),
                None => out.warnings.push("--data-dir needs a directory".into()),
            },
            flag if flag.starts_with('-') && flag.len() > 1 => out.warnings.push(format!("unknown option {flag} ignored")),
            target => match parse_target(target) {
                Ok(t) => out.open = Some(t),
                Err(e) => out.warnings.push(e),
            },
        }
    }
    out
}

/// Interprets a URL or a SQLite file path.
fn parse_target(s: &str) -> Result<OpenTarget, String> {
    if s.contains("://") {
        return parse_url(s).map(|spec| OpenTarget::Url(Box::new(spec))).map_err(|e| e.to_string());
    }
    let path = PathBuf::from(s);
    let is_db = path.extension().and_then(|e| e.to_str()).is_some_and(|e| SQLITE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
    if is_db { Ok(OpenTarget::SqliteFile(path)) } else { Err(format!("'{s}' is neither a connection URL nor a SQLite file")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    /// A URL argument is parsed, including table-open options.
    #[test]
    fn url_argument() {
        let a = parse_args(&args(&["postgresql://postgres@127.0.0.1/acme?table=orders&column=status&operation=Contains&value=shipped"]));
        let Some(OpenTarget::Url(spec)) = a.open else { panic!("no url") };
        assert_eq!(spec.database, "acme");
        assert_eq!(spec.open_table.unwrap().table, "orders");
        assert!(a.warnings.is_empty());
    }

    /// SQLite files are recognised by extension.
    #[test]
    fn sqlite_file_argument() {
        assert_eq!(parse_args(&args(&["/tmp/a.sqlite3"])).open, Some(OpenTarget::SqliteFile(PathBuf::from("/tmp/a.sqlite3"))));
        assert_eq!(parse_args(&args(&["B.DB"])).open, Some(OpenTarget::SqliteFile(PathBuf::from("B.DB"))));
    }

    /// Unknown flags and unusable targets only warn.
    #[test]
    fn warnings_do_not_abort() {
        let a = parse_args(&args(&["--wat", "notes.txt", "oracle://x/y"]));
        assert_eq!(a.open, None);
        assert_eq!(a.warnings.len(), 3);
    }

    /// `--data-dir` and `--help`.
    #[test]
    fn options() {
        let a = parse_args(&args(&["--data-dir", "/x", "-h"]));
        assert_eq!(a.data_dir, Some(PathBuf::from("/x")));
        assert!(a.help);
        assert_eq!(parse_args(&args(&["--data-dir"])).warnings.len(), 1);
    }
}
