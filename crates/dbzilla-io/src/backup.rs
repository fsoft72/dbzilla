//! Backup and restore (SPEC 12.3). PostgreSQL and MySQL/MariaDB spawn the external dump tools found on
//! the `PATH` or in a configured directory; SQLite is archived as a `tar.bz2` of its files.
//! Passwords travel in the environment of the tool, never in its arguments.

use crate::IoError;
use bzip2::read::BzDecoder;
use bzip2::write::BzEncoder;
use bzip2::Compression as BzCompression;
use dbzilla_core::dialect::DriverId;
use dbzilla_core::model::SslMode;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// First bytes of a `pg_dump` custom archive.
const PG_ARCHIVE_MAGIC: &[u8] = b"PGDMP";
/// Files that make up an SQLite database next to the main file.
const SQLITE_SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];
/// Name the main file gets inside the archive, so a restore can rename it freely.
const SQLITE_MAIN_ENTRY: &str = "database";

/// Where and as whom a tool connects.
#[derive(Debug, Clone)]
pub struct Target {
    pub driver: DriverId,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub password: Option<String>,
    pub ssl: SslMode,
}

/// How the tool reads or writes its data file.
#[derive(Debug, Clone, PartialEq)]
pub enum FileIo {
    /// The tool writes to stdout, saved to this file, gzipped when asked.
    SaveStdout { path: PathBuf, gzip: bool },
    /// The tool reads stdin from this file, gunzipped when the name ends in `.gz`.
    FeedStdin { path: PathBuf },
    /// The file is an argument of the tool, nothing is piped.
    None,
}

/// One run of an external tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolPlan {
    /// Name of the executable, without a directory.
    pub tool: &'static str,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub io: FileIo,
}

/// Options offered when the user has not chosen any, per driver.
pub fn default_backup_options(driver: DriverId) -> &'static str {
    match driver {
        DriverId::Postgres => "--format=custom",
        DriverId::MySql => "--single-transaction",
        DriverId::Sqlite => "",
    }
}

/// Restore options offered when the user has not chosen any, per driver.
pub fn default_restore_options(driver: DriverId) -> &'static str {
    match driver {
        DriverId::Postgres => "--single-transaction",
        DriverId::MySql | DriverId::Sqlite => "",
    }
}

/// Options the `Add option…` menu proposes.
pub fn option_choices(driver: DriverId, restore: bool) -> &'static [&'static str] {
    match (driver, restore) {
        (DriverId::Postgres, false) => &["--data-only", "--schema-only", "--clean", "--create", "--no-owner"],
        (DriverId::Postgres, true) => &["--single-transaction", "--clean", "--create", "--exit-on-error", "--no-owner", "--data-only", "--schema-only"],
        (DriverId::MySql, false) => &["--single-transaction", "--no-data", "--no-create-info", "--routines", "--triggers", "--events", "--add-drop-database"],
        (DriverId::MySql, true) => &["--force", "--binary-mode"],
        (DriverId::Sqlite, _) => &[],
    }
}

/// True when the driver is backed up with external tools.
pub fn uses_tools(driver: DriverId) -> bool {
    matches!(driver, DriverId::Postgres | DriverId::MySql)
}

/// Splits free text options into arguments; single and double quotes group words.
pub fn split_options(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut quote, mut any) = (Vec::new(), String::new(), None, false);
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// True when the PostgreSQL options ask for the custom archive format, which is compressed already.
pub fn is_custom_format(options: &[String]) -> bool {
    options.iter().enumerate().any(|(i, o)| matches!(o.as_str(), "--format=custom" | "--format=c" | "-Fc" | "-F=c") || (o == "-F" && options.get(i + 1).is_some_and(|n| n == "c" || n == "custom")))
}

/// Connection arguments and environment shared by the dump and restore tools.
fn connection(t: &Target) -> (Vec<String>, Vec<(String, String)>) {
    let mut args = Vec::new();
    let mut env = Vec::new();
    match t.driver {
        DriverId::Postgres => {
            args.extend(["-h".to_owned(), t.host.clone()]);
            if let Some(p) = t.port {
                args.extend(["-p".to_owned(), p.to_string()]);
            }
            args.extend(["-U".to_owned(), t.user.clone()]);
            if let Some(pw) = &t.password {
                env.push(("PGPASSWORD".to_owned(), pw.clone()));
            }
            let mode = match t.ssl {
                SslMode::Preferred => None,
                SslMode::Disabled => Some("disable"),
                SslMode::Required => Some("require"),
                SslMode::VerifyCa => Some("verify-ca"),
                SslMode::VerifyFull => Some("verify-full"),
            };
            if let Some(m) = mode {
                env.push(("PGSSLMODE".to_owned(), m.to_owned()));
            }
        }
        DriverId::MySql | DriverId::Sqlite => {
            args.extend(["-h".to_owned(), t.host.clone()]);
            if let Some(p) = t.port {
                args.extend(["-P".to_owned(), p.to_string()]);
            }
            args.extend(["-u".to_owned(), t.user.clone()]);
            if let Some(pw) = &t.password {
                env.push(("MYSQL_PWD".to_owned(), pw.clone()));
            }
        }
    }
    (args, env)
}

/// The dump command for `database`, writing to `out`. `gzip` is ignored for the PostgreSQL custom format.
pub fn backup_plan(t: &Target, database: &str, options: &str, out: &Path, gzip: bool) -> Result<ToolPlan, IoError> {
    if database.trim().is_empty() {
        return Err(IoError::Format("choose the database to back up".into()));
    }
    let extra = split_options(options);
    let (mut args, env) = connection(t);
    match t.driver {
        DriverId::Postgres => {
            let gzip = gzip && !is_custom_format(&extra);
            args.extend(["-d".to_owned(), database.to_owned()]);
            args.extend(extra);
            Ok(ToolPlan { tool: "pg_dump", args, env, io: FileIo::SaveStdout { path: out.to_owned(), gzip } })
        }
        DriverId::MySql => {
            args.extend(extra);
            args.push(database.to_owned());
            Ok(ToolPlan { tool: "mysqldump", args, env, io: FileIo::SaveStdout { path: out.to_owned(), gzip } })
        }
        DriverId::Sqlite => Err(IoError::Unsupported("SQLite is archived with sqlite_backup".into())),
    }
}

/// The restore command that loads `file` into `database`. PostgreSQL needs a custom archive; plain
/// SQL files go through the SQL dump import instead.
pub fn restore_plan(t: &Target, database: &str, options: &str, file: &Path) -> Result<ToolPlan, IoError> {
    if database.trim().is_empty() {
        return Err(IoError::Format("choose the database to restore into".into()));
    }
    let extra = split_options(options);
    let (mut args, env) = connection(t);
    match t.driver {
        DriverId::Postgres => {
            let mut magic = [0u8; 5];
            let read = File::open(file)?.read(&mut magic)?;
            if read < magic.len() || magic != *PG_ARCHIVE_MAGIC {
                return Err(IoError::Format("this is not a pg_dump archive: import plain SQL files with File > Import > From SQL Dump".into()));
            }
            args.extend(["-d".to_owned(), database.to_owned()]);
            args.extend(extra);
            args.push(file.to_string_lossy().into_owned());
            Ok(ToolPlan { tool: "pg_restore", args, env, io: FileIo::None })
        }
        DriverId::MySql => {
            args.extend(extra);
            args.push(database.to_owned());
            Ok(ToolPlan { tool: "mysql", args, env, io: FileIo::FeedStdin { path: file.to_owned() } })
        }
        DriverId::Sqlite => Err(IoError::Unsupported("SQLite is restored with sqlite_restore".into())),
    }
}

/// Finds an executable in `dir` first, then in the `PATH`.
pub fn find_tool(name: &str, dir: Option<&Path>) -> Result<PathBuf, IoError> {
    let file = if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() };
    let path_dirs = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default();
    let found = dir.into_iter().map(Path::to_path_buf).chain(path_dirs).map(|d| d.join(&file)).find(|p| p.is_file());
    found.ok_or_else(|| IoError::Format(format!("{name} was not found: install the database client tools or set the tools directory")))
}

/// Receives the lines the tool prints.
pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;

/// Runs the plan to the end, streaming the tool messages to `log`. Blocks, so call it from a thread.
/// A failed or cancelled backup leaves no partial file.
pub fn run_plan(plan: &ToolPlan, tool_dir: Option<&Path>, log: LogFn, cancel: &AtomicBool) -> Result<(), IoError> {
    let program = find_tool(plan.tool, tool_dir)?;
    let mut cmd = Command::new(&program);
    cmd.args(&plan.args).envs(plan.env.iter().map(|(k, v)| (k, v))).stderr(Stdio::piped());
    let mut feed: Option<Box<dyn Read + Send>> = None;
    match &plan.io {
        FileIo::SaveStdout { .. } => {
            cmd.stdout(Stdio::piped()).stdin(Stdio::null());
        }
        FileIo::FeedStdin { path } => {
            let file = File::open(path)?;
            feed = Some(if path.extension().is_some_and(|e| e == "gz") { Box::new(GzDecoder::new(file)) } else { Box::new(file) });
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped());
        }
        FileIo::None => {
            cmd.stdin(Stdio::null()).stdout(Stdio::piped());
        }
    }
    log(format!("$ {} {}", plan.tool, plan.args.join(" ")));
    let mut child = cmd.spawn().map_err(|e| IoError::Format(format!("cannot start {}: {e}", program.display())))?;
    let mut workers = Vec::new();
    if let Some(err) = child.stderr.take() {
        let log = log.clone();
        workers.push(std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                log(line);
            }
            Ok(())
        }));
    }
    if let Some(out) = child.stdout.take() {
        let log = log.clone();
        let save = match &plan.io {
            FileIo::SaveStdout { path, gzip } => Some((path.clone(), *gzip)),
            _ => None,
        };
        workers.push(std::thread::spawn(move || match save {
            Some((path, gzip)) => save_stream(out, &path, gzip),
            None => {
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    log(line);
                }
                Ok(())
            }
        }));
    }
    if let (Some(mut source), Some(mut stdin)) = (feed, child.stdin.take()) {
        workers.push(std::thread::spawn(move || {
            // The tool may stop reading when it fails: its exit status tells why.
            let _ = std::io::copy(&mut source, &mut stdin);
            Ok(())
        }));
    }
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            discard(&plan.io);
            return Err(IoError::Cancelled);
        }
        match child.try_wait()? {
            Some(s) => break s,
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    for w in workers {
        w.join().map_err(|_| IoError::Format("a tool stream thread panicked".into()))??;
    }
    if !status.success() {
        discard(&plan.io);
        return Err(IoError::Format(format!("{} failed with {status}", plan.tool)));
    }
    Ok(())
}

/// Copies the tool output into the backup file, gzipped when asked.
fn save_stream(mut from: impl Read, path: &Path, gzip: bool) -> Result<(), IoError> {
    let file = File::create(path)?;
    if gzip {
        let mut enc = GzEncoder::new(file, Compression::default());
        std::io::copy(&mut from, &mut enc)?;
        enc.finish()?.flush()?;
    } else {
        let mut file = file;
        std::io::copy(&mut from, &mut file)?;
        file.flush()?;
    }
    Ok(())
}

/// Removes the partial backup file of a run that did not finish.
fn discard(io: &FileIo) {
    if let FileIo::SaveStdout { path, .. } = io {
        let _ = std::fs::remove_file(path);
    }
}

/// Paths of the SQLite database and of the sidecar files that exist next to it.
fn sqlite_files(db: &Path) -> Vec<(String, PathBuf)> {
    let mut files = vec![(SQLITE_MAIN_ENTRY.to_owned(), db.to_owned())];
    for suffix in SQLITE_SIDECARS {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        let path = PathBuf::from(name);
        if path.is_file() {
            files.push((format!("{SQLITE_MAIN_ENTRY}{suffix}"), path));
        }
    }
    files
}

/// Writes a `tar.bz2` with the database file and its `-wal`, `-shm` and `-journal` files when present.
/// Returns the number of files archived.
pub fn sqlite_backup(db: &Path, out: &Path) -> Result<usize, IoError> {
    if !db.is_file() {
        return Err(IoError::Format(format!("{} is not a database file", db.display())));
    }
    let files = sqlite_files(db);
    let result = (|| -> Result<(), IoError> {
        let enc = BzEncoder::new(File::create(out)?, BzCompression::default());
        let mut tar = tar::Builder::new(enc);
        for (name, path) in &files {
            tar.append_path_with_name(path, name)?;
        }
        tar.into_inner()?.finish()?.flush()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(out);
        return Err(e);
    }
    Ok(files.len())
}

/// Suffix of an archive entry after the main name (`""`, `-wal`, ...), or an error for unknown entries.
fn entry_suffix(name: &str) -> Result<&str, IoError> {
    let rest = name.strip_prefix(SQLITE_MAIN_ENTRY).ok_or_else(|| IoError::Format(format!("unexpected entry {name} in the backup")))?;
    if rest.is_empty() || SQLITE_SIDECARS.contains(&rest) { Ok(rest) } else { Err(IoError::Format(format!("unexpected entry {name} in the backup"))) }
}

/// Where each file of the archive would be written for the database path `target`.
pub fn sqlite_restore_targets(archive: &Path, target: &Path) -> Result<Vec<PathBuf>, IoError> {
    let mut tar = tar::Archive::new(BzDecoder::new(File::open(archive)?));
    let mut out = Vec::new();
    for entry in tar.entries()? {
        let name = entry?.path()?.to_string_lossy().into_owned();
        let mut path = target.as_os_str().to_owned();
        path.push(entry_suffix(&name)?);
        out.push(PathBuf::from(path));
    }
    if out.is_empty() {
        return Err(IoError::Format("the backup is empty".into()));
    }
    Ok(out)
}

/// Extracts the archive as the database `target`. Existing files are never replaced unless `overwrite`.
pub fn sqlite_restore(archive: &Path, target: &Path, overwrite: bool) -> Result<Vec<PathBuf>, IoError> {
    let targets = sqlite_restore_targets(archive, target)?;
    if !overwrite {
        if let Some(p) = targets.iter().find(|p| p.exists()) {
            return Err(IoError::Format(format!("{} already exists", p.display())));
        }
    }
    let mut tar = tar::Archive::new(BzDecoder::new(File::open(archive)?));
    for (entry, dest) in tar.entries()?.zip(&targets) {
        let mut entry = entry?;
        let mut file = File::create(dest)?;
        std::io::copy(&mut entry, &mut file)?;
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(driver: DriverId) -> Target {
        Target { driver, host: "db.local".into(), port: Some(5433), user: "bob".into(), password: Some("s3cret".into()), ssl: SslMode::Required }
    }

    #[test]
    fn options_split_with_quotes() {
        assert_eq!(split_options("--clean  --table 'my table' -x"), vec!["--clean", "--table", "my table", "-x"]);
        assert!(split_options("   ").is_empty());
        assert_eq!(split_options("--a ''"), vec!["--a", ""]);
    }

    #[test]
    fn postgres_backup_keeps_the_password_out_of_the_arguments() {
        let plan = backup_plan(&target(DriverId::Postgres), "shop", "--format=custom --no-owner", Path::new("/tmp/o.dump"), true).unwrap();
        assert_eq!(plan.tool, "pg_dump");
        assert_eq!(plan.args, vec!["-h", "db.local", "-p", "5433", "-U", "bob", "-d", "shop", "--format=custom", "--no-owner"]);
        assert!(!plan.args.iter().any(|a| a.contains("s3cret")));
        assert!(plan.env.contains(&("PGPASSWORD".into(), "s3cret".into())));
        assert!(plan.env.contains(&("PGSSLMODE".into(), "require".into())));
        // The custom format is compressed already, so gzip is dropped.
        assert_eq!(plan.io, FileIo::SaveStdout { path: "/tmp/o.dump".into(), gzip: false });
    }

    #[test]
    fn plain_postgres_backup_can_be_gzipped() {
        let plan = backup_plan(&target(DriverId::Postgres), "shop", "--clean", Path::new("o.sql.gz"), true).unwrap();
        assert_eq!(plan.io, FileIo::SaveStdout { path: "o.sql.gz".into(), gzip: true });
    }

    #[test]
    fn mysql_backup_and_restore_use_the_environment_password() {
        let b = backup_plan(&target(DriverId::MySql), "shop", "--single-transaction", Path::new("o.sql"), false).unwrap();
        assert_eq!(b.tool, "mysqldump");
        assert_eq!(b.args, vec!["-h", "db.local", "-P", "5433", "-u", "bob", "--single-transaction", "shop"]);
        assert_eq!(b.env, vec![("MYSQL_PWD".to_owned(), "s3cret".to_owned())]);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("o.sql");
        std::fs::write(&file, "select 1;").unwrap();
        let r = restore_plan(&target(DriverId::MySql), "shop", "--force", &file).unwrap();
        assert_eq!(r.tool, "mysql");
        assert_eq!(r.io, FileIo::FeedStdin { path: file });
        assert_eq!(r.args.last().map(String::as_str), Some("shop"));
    }

    #[test]
    fn pg_restore_refuses_plain_sql() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("a.sql");
        std::fs::write(&plain, "CREATE TABLE t();").unwrap();
        let err = restore_plan(&target(DriverId::Postgres), "shop", "", &plain).unwrap_err().to_string();
        assert!(err.contains("Import"), "{err}");
        let archive = dir.path().join("a.dump");
        std::fs::write(&archive, b"PGDMP\x01\x0e").unwrap();
        let plan = restore_plan(&target(DriverId::Postgres), "shop", "--single-transaction", &archive).unwrap();
        assert_eq!(plan.tool, "pg_restore");
        assert_eq!(plan.args.last().map(String::as_str), archive.to_str());
    }

    #[test]
    fn empty_database_is_refused() {
        assert!(backup_plan(&target(DriverId::Postgres), " ", "", Path::new("o"), false).is_err());
    }

    #[test]
    fn missing_tool_gives_an_install_hint() {
        let err = find_tool("dbzilla-no-such-tool", None).unwrap_err().to_string();
        assert!(err.contains("not found") || err.contains("was not found"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn run_plan_saves_stdout_and_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("o.txt");
        let ok = ToolPlan { tool: "echo", args: vec!["hello".into()], env: vec![], io: FileIo::SaveStdout { path: out.clone(), gzip: false } };
        run_plan(&ok, None, Arc::new(|_| {}), &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "hello\n");
        let bad = ToolPlan { tool: "false", args: vec![], env: vec![], io: FileIo::SaveStdout { path: out.clone(), gzip: false } };
        assert!(run_plan(&bad, None, Arc::new(|_| {}), &AtomicBool::new(false)).is_err());
        assert!(!out.exists(), "a failed backup leaves no file");
    }

    #[cfg(unix)]
    #[test]
    fn cancel_stops_the_tool() {
        let dir = tempfile::tempdir().unwrap();
        let plan = ToolPlan { tool: "sleep", args: vec!["30".into()], env: vec![], io: FileIo::SaveStdout { path: dir.path().join("o"), gzip: false } };
        let cancel = AtomicBool::new(true);
        assert!(matches!(run_plan(&plan, None, Arc::new(|_| {}), &cancel), Err(IoError::Cancelled)));
    }

    fn sqlite_fixture(dir: &Path) -> PathBuf {
        let db = dir.join("app.sqlite3");
        std::fs::write(&db, b"main").unwrap();
        std::fs::write(dir.join("app.sqlite3-wal"), b"wal").unwrap();
        db
    }

    #[test]
    fn sqlite_backup_roundtrip_keeps_the_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let db = sqlite_fixture(dir.path());
        let archive = dir.path().join("b.tar.bz2");
        assert_eq!(sqlite_backup(&db, &archive).unwrap(), 2);
        let target = dir.path().join("copy.sqlite3");
        let written = sqlite_restore(&archive, &target, false).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(std::fs::read(&target).unwrap(), b"main");
        assert_eq!(std::fs::read(dir.path().join("copy.sqlite3-wal")).unwrap(), b"wal");
    }

    #[test]
    fn sqlite_restore_never_overwrites_without_consent() {
        let dir = tempfile::tempdir().unwrap();
        let db = sqlite_fixture(dir.path());
        let archive = dir.path().join("b.tar.bz2");
        sqlite_backup(&db, &archive).unwrap();
        std::fs::write(&db, b"changed").unwrap();
        let err = sqlite_restore(&archive, &db, false).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&db).unwrap(), b"changed");
        assert_eq!(sqlite_restore_targets(&archive, &db).unwrap().len(), 2);
        sqlite_restore(&archive, &db, true).unwrap();
        assert_eq!(std::fs::read(&db).unwrap(), b"main");
    }

    #[test]
    fn sqlite_backup_of_a_missing_file_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("b.tar.bz2");
        assert!(sqlite_backup(&dir.path().join("none.db"), &out).is_err());
        assert!(!out.exists());
    }
}
