//! Connection URL parse/format tests (SPEC 6.3).

use dbzilla_core::dialect::DriverId;
use dbzilla_core::url::{format_url, parse_url, UrlError};

#[test]
fn parses_full_postgres_url() {
    let s = parse_url("postgresql://postgres:s%40cret@127.0.0.1:5433/acme?name=Prod&statusColor=FF0000&env=production&tLSMode=1&safeModeLevel=2").unwrap();
    assert_eq!(s.driver, DriverId::Postgres);
    assert_eq!(s.user, "postgres");
    assert_eq!(s.password.as_deref(), Some("s@cret"));
    assert_eq!(s.host, "127.0.0.1");
    assert_eq!(s.port, Some(5433));
    assert_eq!(s.database, "acme");
    assert_eq!(s.name.as_deref(), Some("Prod"));
    assert_eq!(s.status_color.as_deref(), Some("FF0000"));
    assert_eq!(s.env.as_deref(), Some("production"));
    assert_eq!(s.tls_mode, Some(1));
    assert_eq!(s.safe_level, Some(2));
}

#[test]
fn scheme_aliases() {
    assert_eq!(parse_url("postgres://h/db").unwrap().driver, DriverId::Postgres);
    assert_eq!(parse_url("mysql://h/db").unwrap().driver, DriverId::MySql);
    assert_eq!(parse_url("mariadb://h/db").unwrap().driver, DriverId::MySql);
}

#[test]
fn format_omits_default_port_and_hides_password_on_request() {
    let s = parse_url("postgresql://u:pw@h:5432/db").unwrap();
    assert_eq!(format_url(&s, true), "postgresql://u:pw@h/db");
    assert_eq!(format_url(&s, false), "postgresql://u@h/db");
}

#[test]
fn round_trip_with_special_characters() {
    let mut s = parse_url("mysql://u@h/db").unwrap();
    s.password = Some("p@ss:w/rd#?%".into());
    s.name = Some("My DB & co".into());
    let text = format_url(&s, true);
    let back = parse_url(&text).unwrap();
    assert_eq!(back, s);
}

#[test]
fn ipv6_host() {
    let s = parse_url("postgresql://u@[::1]:5433/db").unwrap();
    assert_eq!(s.host, "::1");
    assert_eq!(format_url(&s, false), "postgresql://u@[::1]:5433/db");
}

#[test]
fn sqlite_file_paths() {
    let s = parse_url("sqlite:///home/me/my%20db.sqlite").unwrap();
    assert_eq!(s.driver, DriverId::Sqlite);
    assert_eq!(s.database, "/home/me/my db.sqlite");
    assert_eq!(format_url(&s, false), "sqlite:///home/me/my%20db.sqlite");
}

#[test]
fn open_table_options_and_aliases() {
    let s = parse_url("postgresql://h/db?schema=public&table=orders&column=status&operation=Contains&value=shipped").unwrap();
    let t = s.open_table.unwrap();
    assert_eq!(t.schema.as_deref(), Some("public"));
    assert_eq!(t.table, "orders");
    assert_eq!(t.column.as_deref(), Some("status"));
    assert_eq!(t.operation.as_deref(), Some("Contains"));
    assert_eq!(t.value.as_deref(), Some("shipped"));
    let s = parse_url("mysql://h/db?view=v&raw=total%20%3E%20100").unwrap();
    let t = s.open_table.unwrap();
    assert_eq!(t.table, "v");
    assert_eq!(t.condition.as_deref(), Some("total > 100"));
}

#[test]
fn unknown_unsupported_and_invalid() {
    assert!(matches!(parse_url("oracle://h/db"), Err(UrlError::UnknownScheme(s)) if s == "oracle"));
    assert!(matches!(parse_url("postgresql+ssh://u@h/x"), Err(UrlError::Unsupported(_))));
    assert!(matches!(parse_url("not a url"), Err(UrlError::Invalid(_))));
    assert!(matches!(parse_url("postgresql://h/db?tLSMode=abc"), Err(UrlError::Invalid(_))));
}

#[test]
fn unknown_options_are_preserved() {
    let s = parse_url("mysql://h/db?lazyload=1").unwrap();
    assert_eq!(s.extra.get("lazyload").map(String::as_str), Some("1"));
    assert!(format_url(&s, false).contains("lazyload=1"));
}
