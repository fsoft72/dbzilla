//! User management (SPEC 11.5): the model of a database user and the diff that turns an edited user
//! into `CREATE`, `ALTER`, `GRANT`, `REVOKE` and `DROP` statements. Pure logic, no UI and no I/O.

use crate::dialect::{dialect_for, DriverId};
use crate::model::Row;
use crate::value::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Text that replaces a password in what is shown to the user.
pub const MASK: &str = "'********'";

/// Role attributes of PostgreSQL, in the order the editor lists them: `(keyword, label)`.
pub const PG_ATTRIBUTES: [(&str, &str); 4] = [("CREATEDB", "Can create databases"), ("SUPERUSER", "Superuser"), ("REPLICATION", "Replication"), ("BYPASSRLS", "Bypass RLS")];
/// Privileges that can be granted on a PostgreSQL table.
pub const PG_TABLE_PRIVILEGES: [&str; 7] = ["SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER"];
/// Global privileges of MySQL and MariaDB, by group.
pub const MYSQL_GLOBAL_GROUPS: [(&str, &[&str]); 3] = [
    ("Database and tables", &["ALTER", "CREATE", "CREATE TEMPORARY TABLES", "DELETE", "DROP", "INDEX", "INSERT", "LOCK TABLES", "REFERENCES", "SELECT", "UPDATE"]),
    ("Views and procedures", &["ALTER ROUTINE", "CREATE ROUTINE", "CREATE VIEW", "EVENT", "EXECUTE", "SHOW VIEW", "TRIGGER"]),
    ("Admin", &["CREATE USER", "FILE", "PROCESS", "RELOAD", "REPLICATION CLIENT", "REPLICATION SLAVE", "SHOW DATABASES", "SHUTDOWN", "SUPER"]),
];

/// Privileges that make sense on a single MySQL database: the first two global groups.
pub fn mysql_database_privileges() -> Vec<&'static str> {
    MYSQL_GLOBAL_GROUPS[..2].iter().flat_map(|(_, p)| p.iter().copied()).collect()
}

/// Every global privilege of MySQL and MariaDB.
pub fn mysql_global_privileges() -> Vec<&'static str> {
    MYSQL_GLOBAL_GROUPS.iter().flat_map(|(_, p)| p.iter().copied()).collect()
}

/// Per hour limits of a MySQL or MariaDB account; 0 means no limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    pub max_updates: u32,
    pub max_connections: u32,
    pub max_questions: u32,
}

/// A database user as the editor shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserModel {
    pub driver: DriverId,
    pub name: String,
    /// Host part of a MySQL account (`localhost`, `%`); empty for PostgreSQL.
    pub host: String,
    /// New password; `None` keeps the current one.
    pub password: Option<String>,
    /// PostgreSQL role attributes or MySQL global privileges, in upper case.
    pub globals: BTreeSet<String>,
    /// Privileges by scope: `schema.table` for PostgreSQL, the database name for MySQL.
    pub scoped: BTreeMap<String, BTreeSet<String>>,
    pub limits: Limits,
}

impl UserModel {
    /// An empty user for the driver.
    pub fn new(driver: DriverId, name: &str) -> Self {
        let host = if driver == DriverId::MySql { "localhost".to_owned() } else { String::new() };
        Self { driver, name: name.to_owned(), host, password: None, globals: BTreeSet::new(), scoped: BTreeMap::new(), limits: Limits::default() }
    }

    /// Unique key of the user in a list: `name` or `name@host`.
    pub fn key(&self) -> String {
        if self.host.is_empty() { self.name.clone() } else { format!("{}@{}", self.name, self.host) }
    }

    /// The account in SQL: `'name'@'host'` for MySQL, the quoted role name for PostgreSQL.
    fn account(&self) -> String {
        match self.driver {
            DriverId::MySql => format!("{}@{}", text(self.driver, &self.name), text(self.driver, &self.host)),
            _ => dialect_for(self.driver).quote_ident(&self.name),
        }
    }
}

/// A string literal of the dialect.
fn text(driver: DriverId, s: &str) -> String {
    dialect_for(driver).literal(&Value::Text(s.to_owned()))
}

/// The target of a grant for a scope key.
fn scope_sql(driver: DriverId, scope: &str) -> String {
    let d = dialect_for(driver);
    match driver {
        DriverId::MySql if scope == "*" => "*.*".to_owned(),
        DriverId::MySql => format!("{}.*", d.quote_ident(scope)),
        _ => match scope.split_once('.') {
            Some((schema, table)) => format!("TABLE {}.{}", d.quote_ident(schema), d.quote_ident(table)),
            None => format!("TABLE {}", d.quote_ident(scope)),
        },
    }
}

/// `GRANT` or `REVOKE` statement, when `privileges` is not empty.
fn grant_or_revoke(grant: bool, privileges: &BTreeSet<String>, target: &str, account: &str) -> Option<String> {
    if privileges.is_empty() {
        return None;
    }
    let list = privileges.iter().map(String::as_str).collect::<Vec<_>>().join(", ");
    Some(if grant { format!("GRANT {list} ON {target} TO {account};") } else { format!("REVOKE {list} ON {target} FROM {account};") })
}

/// Statements for the limits of a MySQL account.
fn limits_sql(u: &UserModel) -> String {
    format!("ALTER USER {} WITH MAX_UPDATES_PER_HOUR {} MAX_CONNECTIONS_PER_HOUR {} MAX_QUERIES_PER_HOUR {};", u.account(), u.limits.max_updates, u.limits.max_connections, u.limits.max_questions)
}

/// The statements that change `original` into `edited`, or create `edited` when there is no original.
/// Passwords are written as they are: use [`mask_passwords`] before showing them.
pub fn diff(original: Option<&UserModel>, edited: &UserModel) -> Vec<String> {
    let mut out = Vec::new();
    let account = edited.account();
    let pw = edited.password.as_deref().filter(|p| !p.is_empty()).map(|p| text(edited.driver, p));
    match edited.driver {
        DriverId::Postgres => {
            let attrs = |on: &BTreeSet<String>, off: &BTreeSet<String>| -> Vec<String> {
                PG_ATTRIBUTES.iter().filter_map(|(k, _)| if on.contains(*k) { Some((*k).to_owned()) } else if off.contains(*k) { Some(format!("NO{k}")) } else { None }).collect()
            };
            match original {
                None => {
                    let mut words = vec!["LOGIN".to_owned()];
                    words.extend(attrs(&edited.globals, &BTreeSet::new()));
                    if let Some(p) = &pw {
                        words.push(format!("PASSWORD {p}"));
                    }
                    out.push(format!("CREATE ROLE {account} WITH {};", words.join(" ")));
                }
                Some(o) => {
                    let turned_on: BTreeSet<String> = edited.globals.difference(&o.globals).cloned().collect();
                    let turned_off: BTreeSet<String> = o.globals.difference(&edited.globals).cloned().collect();
                    let mut words = attrs(&turned_on, &turned_off);
                    if let Some(p) = &pw {
                        words.push(format!("PASSWORD {p}"));
                    }
                    if !words.is_empty() {
                        out.push(format!("ALTER ROLE {account} WITH {};", words.join(" ")));
                    }
                }
            }
        }
        _ => {
            match original {
                None => out.push(match &pw {
                    Some(p) => format!("CREATE USER {account} IDENTIFIED BY {p};"),
                    None => format!("CREATE USER {account};"),
                }),
                Some(_) => {
                    if let Some(p) = &pw {
                        out.push(format!("ALTER USER {account} IDENTIFIED BY {p};"));
                    }
                }
            }
            let old_limits = original.map(|o| o.limits).unwrap_or_default();
            if edited.limits != old_limits {
                out.push(limits_sql(edited));
            }
        }
    }
    let empty = BTreeSet::new();
    if edited.driver == DriverId::MySql {
        let before = original.map_or(&empty, |o| &o.globals);
        let target = scope_sql(edited.driver, "*");
        out.extend(grant_or_revoke(false, &before.difference(&edited.globals).cloned().collect(), &target, &account));
        out.extend(grant_or_revoke(true, &edited.globals.difference(before).cloned().collect(), &target, &account));
    }
    let scopes: BTreeSet<&String> = edited.scoped.keys().chain(original.into_iter().flat_map(|o| o.scoped.keys())).collect();
    for scope in scopes {
        let before = original.and_then(|o| o.scoped.get(scope)).unwrap_or(&empty);
        let after = edited.scoped.get(scope).unwrap_or(&empty);
        let target = scope_sql(edited.driver, scope);
        out.extend(grant_or_revoke(false, &before.difference(after).cloned().collect(), &target, &account));
        out.extend(grant_or_revoke(true, &after.difference(before).cloned().collect(), &target, &account));
    }
    out
}

/// The statements that remove the user. PostgreSQL refuses to drop a role that still has privileges,
/// so those it is known to hold are revoked first.
pub fn drop_statements(u: &UserModel) -> Vec<String> {
    match u.driver {
        DriverId::MySql => vec![format!("DROP USER {};", u.account())],
        _ => {
            let account = u.account();
            let mut out: Vec<String> = u.scoped.iter().filter_map(|(scope, privs)| grant_or_revoke(false, privs, &scope_sql(u.driver, scope), &account)).collect();
            out.push(format!("DROP ROLE {account};"));
            out
        }
    }
}

/// Replaces the password literals of the statements with [`MASK`], for display and history.
pub fn mask_passwords(statements: &[String], edited: &[UserModel]) -> Vec<String> {
    let secrets: Vec<String> = edited.iter().filter_map(|u| u.password.as_deref().filter(|p| !p.is_empty()).map(|p| text(u.driver, p))).collect();
    statements.iter().map(|s| secrets.iter().fold(s.clone(), |acc, secret| acc.replace(secret.as_str(), MASK))).collect()
}

/// Statement that lists the users of the server, with `name`, `host` and the limits for MySQL.
pub fn list_sql(driver: DriverId) -> Option<&'static str> {
    match driver {
        DriverId::Postgres => Some("SELECT rolname, rolsuper, rolcreatedb, rolreplication, rolbypassrls FROM pg_roles WHERE rolname !~ '^pg_' AND rolcanlogin ORDER BY rolname"),
        DriverId::MySql => Some("SELECT user, host, max_updates, max_connections, max_questions FROM mysql.user WHERE user <> '' ORDER BY user, host"),
        DriverId::Sqlite | DriverId::SqlServer => None,
    }
}

/// True when the driver has user management.
pub fn supported(driver: DriverId) -> bool {
    list_sql(driver).is_some()
}

/// Reads the rows of [`list_sql`] into users with no table privileges yet.
pub fn parse_list(driver: DriverId, rows: &[Row]) -> Vec<UserModel> {
    let cell = |r: &Row, i: usize| r.get(i).map(Value::to_display).unwrap_or_default();
    let flag = |r: &Row, i: usize| matches!(cell(r, i).to_lowercase().as_str(), "true" | "t" | "1");
    let num = |r: &Row, i: usize| cell(r, i).parse::<u32>().unwrap_or(0);
    rows.iter()
        .map(|r| {
            let mut u = UserModel::new(driver, &cell(r, 0));
            if driver == DriverId::MySql {
                u.host = cell(r, 1);
                u.limits = Limits { max_updates: num(r, 2), max_connections: num(r, 3), max_questions: num(r, 4) };
            } else {
                for (i, k) in [(1, "SUPERUSER"), (2, "CREATEDB"), (3, "REPLICATION"), (4, "BYPASSRLS")] {
                    if flag(r, i) {
                        u.globals.insert(k.to_owned());
                    }
                }
            }
            u
        })
        .collect()
}

/// Statement that lists the table privileges of a PostgreSQL role: `schema, table, privilege`.
pub fn pg_grants_sql(role: &str) -> String {
    format!(
        "SELECT n.nspname, c.relname, a.privilege_type FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         CROSS JOIN LATERAL aclexplode(c.relacl) a JOIN pg_roles r ON r.oid = a.grantee \
         WHERE r.rolname = {} AND c.relkind IN ('r','p','v','m','f') ORDER BY 1, 2, 3",
        text(DriverId::Postgres, role)
    )
}

/// Statement that lists the tables a PostgreSQL role could get privileges on: `schema, table`.
pub const PG_TABLES_SQL: &str = "SELECT n.nspname, c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE c.relkind IN ('r','p','v','m','f') AND n.nspname NOT IN ('pg_catalog','information_schema') AND n.nspname NOT LIKE 'pg_toast%' ORDER BY 1, 2";

/// Adds the rows of [`pg_grants_sql`] to the user.
pub fn apply_pg_grants(u: &mut UserModel, rows: &[Row]) {
    for r in rows {
        let cell = |i: usize| r.get(i).map(Value::to_display).unwrap_or_default();
        let p = cell(2).to_uppercase();
        if PG_TABLE_PRIVILEGES.contains(&p.as_str()) {
            u.scoped.entry(format!("{}.{}", cell(0), cell(1))).or_default().insert(p);
        }
    }
}

/// Statement that shows the grants of a MySQL account.
pub fn mysql_grants_sql(u: &UserModel) -> String {
    format!("SHOW GRANTS FOR {}", u.account())
}

/// Adds the lines of `SHOW GRANTS` to the user: global privileges and per database ones. Grants on
/// single tables are not editable and are left alone.
pub fn apply_mysql_grants(u: &mut UserModel, lines: &[String]) {
    for line in lines {
        let Some(rest) = line.strip_prefix("GRANT ") else { continue };
        let Some((privs, after)) = rest.split_once(" ON ") else { continue };
        let target = after.split(" TO ").next().unwrap_or("").trim();
        let list: Vec<String> = if privs.trim() == "ALL PRIVILEGES" {
            if target == "*.*" { mysql_global_privileges() } else { mysql_database_privileges() }.into_iter().map(str::to_owned).collect()
        } else {
            privs.split(',').map(|p| p.trim().to_uppercase()).filter(|p| p != "USAGE" && p != "GRANT OPTION").collect()
        };
        if target == "*.*" {
            u.globals.extend(list);
        } else if let Some(db) = target.strip_suffix(".*") {
            u.scoped.entry(db.trim_matches('`').replace("``", "`")).or_default().extend(list);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn pg(name: &str) -> UserModel {
        UserModel::new(DriverId::Postgres, name)
    }

    fn my(name: &str) -> UserModel {
        UserModel::new(DriverId::MySql, name)
    }

    #[test]
    fn new_postgres_role_has_login_attributes_password_and_grants() {
        let mut u = pg("app");
        u.password = Some("it's".into());
        u.globals = set(&["CREATEDB"]);
        u.scoped.insert("public.orders".into(), set(&["INSERT", "SELECT"]));
        assert_eq!(
            diff(None, &u),
            vec!["CREATE ROLE \"app\" WITH LOGIN CREATEDB PASSWORD 'it''s';", "GRANT INSERT, SELECT ON TABLE \"public\".\"orders\" TO \"app\";"]
        );
    }

    #[test]
    fn postgres_edit_changes_only_what_differs() {
        let mut before = pg("app");
        before.globals = set(&["CREATEDB", "SUPERUSER"]);
        before.scoped.insert("public.orders".into(), set(&["SELECT", "DELETE"]));
        assert!(diff(Some(&before), &before).is_empty());
        let mut after = before.clone();
        after.globals = set(&["CREATEDB", "REPLICATION"]);
        after.scoped.insert("public.orders".into(), set(&["SELECT", "INSERT"]));
        after.scoped.insert("public.items".into(), set(&["SELECT"]));
        assert_eq!(
            diff(Some(&before), &after),
            vec![
                "ALTER ROLE \"app\" WITH NOSUPERUSER REPLICATION;",
                "GRANT SELECT ON TABLE \"public\".\"items\" TO \"app\";",
                "REVOKE DELETE ON TABLE \"public\".\"orders\" FROM \"app\";",
                "GRANT INSERT ON TABLE \"public\".\"orders\" TO \"app\";",
            ]
        );
    }

    #[test]
    fn password_alone_alters_the_role() {
        let before = pg("app");
        let mut after = before.clone();
        after.password = Some("n".into());
        assert_eq!(diff(Some(&before), &after), vec!["ALTER ROLE \"app\" WITH PASSWORD 'n';"]);
        after.password = Some(String::new());
        assert!(diff(Some(&before), &after).is_empty(), "an empty password keeps the current one");
    }

    #[test]
    fn new_mysql_account_with_limits_and_privileges() {
        let mut u = my("bob");
        u.host = "%".into();
        u.password = Some("pw".into());
        u.limits = Limits { max_updates: 10, max_connections: 0, max_questions: 500 };
        u.globals = set(&["PROCESS"]);
        u.scoped.insert("shop".into(), set(&["SELECT", "INSERT"]));
        assert_eq!(
            diff(None, &u),
            vec![
                "CREATE USER 'bob'@'%' IDENTIFIED BY 'pw';",
                "ALTER USER 'bob'@'%' WITH MAX_UPDATES_PER_HOUR 10 MAX_CONNECTIONS_PER_HOUR 0 MAX_QUERIES_PER_HOUR 500;",
                "GRANT PROCESS ON *.* TO 'bob'@'%';",
                "GRANT INSERT, SELECT ON `shop`.* TO 'bob'@'%';",
            ]
        );
    }

    #[test]
    fn mysql_revoke_and_unchanged_limits() {
        let mut before = my("bob");
        before.globals = set(&["PROCESS", "RELOAD"]);
        before.scoped.insert("shop".into(), set(&["SELECT"]));
        let mut after = before.clone();
        after.globals = set(&["RELOAD"]);
        after.scoped.clear();
        assert_eq!(diff(Some(&before), &after), vec!["REVOKE PROCESS ON *.* FROM 'bob'@'localhost';", "REVOKE SELECT ON `shop`.* FROM 'bob'@'localhost';"]);
    }

    #[test]
    fn names_are_quoted_against_injection() {
        let u = my("a'; DROP USER x; --");
        assert_eq!(drop_statements(&u), vec!["DROP USER 'a''; DROP USER x; --'@'localhost';"]);
        assert_eq!(drop_statements(&pg("we\"ird")), vec!["DROP ROLE \"we\"\"ird\";"]);
    }

    #[test]
    fn dropping_a_postgres_role_revokes_its_table_privileges_first() {
        let mut u = pg("app");
        u.scoped.insert("public.orders".into(), set(&["SELECT"]));
        assert_eq!(drop_statements(&u), vec!["REVOKE SELECT ON TABLE \"public\".\"orders\" FROM \"app\";", "DROP ROLE \"app\";"]);
    }

    #[test]
    fn passwords_are_masked_for_display() {
        let mut u = pg("app");
        u.password = Some("secret".into());
        let stmts = diff(None, &u);
        let shown = mask_passwords(&stmts, &[u]);
        assert_eq!(shown, vec![format!("CREATE ROLE \"app\" WITH LOGIN PASSWORD {MASK};")]);
        assert!(!shown[0].contains("secret"));
    }

    #[test]
    fn show_grants_is_read_into_the_model() {
        let mut u = my("bob");
        apply_mysql_grants(
            &mut u,
            &[
                "GRANT USAGE ON *.* TO `bob`@`%`".to_owned(),
                "GRANT SELECT, PROCESS ON *.* TO `bob`@`%` WITH GRANT OPTION".to_owned(),
                "GRANT ALL PRIVILEGES ON `shop`.* TO `bob`@`%`".to_owned(),
                "GRANT SELECT ON `shop`.`orders` TO `bob`@`%`".to_owned(),
            ],
        );
        assert_eq!(u.globals, set(&["SELECT", "PROCESS"]));
        assert_eq!(u.scoped.len(), 1);
        assert!(u.scoped["shop"].contains("CREATE VIEW") && !u.scoped["shop"].contains("SUPER"));
    }

    #[test]
    fn list_rows_become_users() {
        let rows = vec![vec![Value::Text("bob".into()), Value::Text("%".into()), Value::I64(5), Value::I64(0), Value::I64(7)]];
        let users = parse_list(DriverId::MySql, &rows);
        assert_eq!((users[0].key(), users[0].limits.max_questions), ("bob@%".to_owned(), 7));
        let pg_rows = vec![vec![Value::Text("app".into()), Value::Bool(true), Value::Bool(false), Value::Bool(false), Value::Bool(true)]];
        assert_eq!(parse_list(DriverId::Postgres, &pg_rows)[0].globals, set(&["SUPERUSER", "BYPASSRLS"]));
    }

    #[test]
    fn pg_grants_fill_scopes() {
        let mut u = pg("app");
        apply_pg_grants(&mut u, &[vec![Value::Text("public".into()), Value::Text("t".into()), Value::Text("SELECT".into())], vec![Value::Text("public".into()), Value::Text("t".into()), Value::Text("USAGE".into())]]);
        assert_eq!(u.scoped["public.t"], set(&["SELECT"]));
        assert!(pg_grants_sql("o'x").contains("'o''x'"));
    }
}
