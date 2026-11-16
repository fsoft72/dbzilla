//! DDL generation and copy-script tests (SPEC 8.6-8.10, appendix A).

use dbzilla_core::ddl::{generate_ddl, ColumnDef, DdlError, ForeignKeyDef, IndexDef, StructuralOp};
use dbzilla_core::dialect::{dialect_for, DriverId};
use dbzilla_core::model::{ColumnMeta, IndexInfo, TableRef, TableStructure};

fn col(name: &str, ty: &str, nullable: bool, default: Option<&str>) -> ColumnDef {
    ColumnDef { name: name.into(), type_name: ty.into(), nullable, default: default.map(str::to_owned), extra: None }
}

fn ddl(id: DriverId, op: StructuralOp) -> Result<Vec<String>, DdlError> {
    generate_ddl(&op, dialect_for(id).as_ref())
}

fn t(name: &str) -> TableRef {
    TableRef::simple(name)
}

fn create_users() -> StructuralOp {
    StructuralOp::CreateTable {
        table: t("users"),
        columns: vec![col("id", "integer", false, None), col("name", "text", true, Some("'x'"))],
        primary_key: vec!["id".into()],
        foreign_keys: vec![ForeignKeyDef {
            name: None,
            columns: vec!["id".into()],
            ref_table: t("accounts"),
            ref_columns: vec!["uid".into()],
            on_update: "NO ACTION".into(),
            on_delete: "CASCADE".into(),
        }],
    }
}

#[test]
fn create_table_postgres() {
    assert_eq!(
        ddl(DriverId::Postgres, create_users()).unwrap(),
        vec!["CREATE TABLE \"users\" (\n  \"id\" integer NOT NULL,\n  \"name\" text DEFAULT 'x',\n  PRIMARY KEY (\"id\"),\n  FOREIGN KEY (\"id\") REFERENCES \"accounts\" (\"uid\") ON UPDATE NO ACTION ON DELETE CASCADE\n)"]
    );
}

#[test]
fn rename_table_per_dialect() {
    let op = || StructuralOp::RenameTable { table: t("a"), new_name: "b".into() };
    assert_eq!(ddl(DriverId::Postgres, op()).unwrap(), vec!["ALTER TABLE \"a\" RENAME TO \"b\""]);
    assert_eq!(ddl(DriverId::MySql, op()).unwrap(), vec!["RENAME TABLE `a` TO `b`"]);
}

#[test]
fn drop_and_truncate_per_dialect() {
    let drop = |cascade| StructuralOp::DropTable { table: t("a"), cascade };
    assert_eq!(ddl(DriverId::Postgres, drop(true)).unwrap(), vec!["DROP TABLE \"a\" CASCADE"]);
    assert_eq!(ddl(DriverId::MySql, drop(true)).unwrap(), vec!["DROP TABLE `a`"]);
    let trunc = || StructuralOp::Truncate { table: t("a"), cascade: true, restart_identity: true };
    assert_eq!(ddl(DriverId::Postgres, trunc()).unwrap(), vec!["TRUNCATE TABLE \"a\" RESTART IDENTITY CASCADE"]);
    assert_eq!(ddl(DriverId::MySql, trunc()).unwrap(), vec!["TRUNCATE TABLE `a`"]);
    assert_eq!(ddl(DriverId::Sqlite, trunc()).unwrap(), vec!["DELETE FROM \"a\""]);
}

#[test]
fn add_and_drop_column() {
    let add = StructuralOp::AddColumn { table: t("a"), column: col("c", "int", false, Some("0")) };
    assert_eq!(ddl(DriverId::MySql, add).unwrap(), vec!["ALTER TABLE `a` ADD COLUMN `c` int NOT NULL DEFAULT 0"]);
    let drop = StructuralOp::DropColumn { table: t("a"), column: "c".into() };
    assert_eq!(ddl(DriverId::Postgres, drop).unwrap(), vec!["ALTER TABLE \"a\" DROP COLUMN \"c\""]);
}

#[test]
fn alter_column_postgres_emits_only_differences() {
    let op = StructuralOp::AlterColumn {
        table: t("a"),
        old: col("c", "integer", true, None),
        new: col("d", "bigint", false, Some("1")),
        current: None,
    };
    assert_eq!(
        ddl(DriverId::Postgres, op).unwrap(),
        vec![
            "ALTER TABLE \"a\" RENAME COLUMN \"c\" TO \"d\"",
            "ALTER TABLE \"a\" ALTER COLUMN \"d\" TYPE bigint",
            "ALTER TABLE \"a\" ALTER COLUMN \"d\" SET NOT NULL",
            "ALTER TABLE \"a\" ALTER COLUMN \"d\" SET DEFAULT 1",
        ]
    );
}

#[test]
fn alter_column_mysql_uses_change_or_modify() {
    let rename = StructuralOp::AlterColumn { table: t("a"), old: col("c", "int", true, None), new: col("d", "bigint", true, None), current: None };
    assert_eq!(ddl(DriverId::MySql, rename).unwrap(), vec!["ALTER TABLE `a` CHANGE COLUMN `c` `d` bigint"]);
    let modify = StructuralOp::AlterColumn { table: t("a"), old: col("c", "int", true, None), new: col("c", "bigint", true, None), current: None };
    assert_eq!(ddl(DriverId::MySql, modify).unwrap(), vec!["ALTER TABLE `a` MODIFY COLUMN `c` bigint"]);
}

/// Review I7: a MySQL MODIFY keeps AUTO_INCREMENT and ON UPDATE.
#[test]
fn mysql_modify_keeps_auto_increment_and_on_update() {
    let mut new = col("id", "bigint", false, None);
    new.extra = dbzilla_core::ddl::mysql_column_suffix("auto_increment");
    let op = StructuralOp::AlterColumn { table: t("a"), old: col("id", "int", false, None), new, current: None };
    assert_eq!(ddl(DriverId::MySql, op).unwrap(), vec!["ALTER TABLE `a` MODIFY COLUMN `id` bigint NOT NULL AUTO_INCREMENT"]);
    assert_eq!(dbzilla_core::ddl::mysql_column_suffix("DEFAULT_GENERATED on update CURRENT_TIMESTAMP(3)").as_deref(), Some("ON UPDATE CURRENT_TIMESTAMP(3)"));
    assert_eq!(dbzilla_core::ddl::mysql_column_suffix(""), None);
}

fn current_structure() -> TableStructure {
    let c = |name: &str, ty: &str, pk: bool| ColumnMeta { name: name.into(), type_name: ty.into(), nullable: !pk, is_pk: pk, ..Default::default() };
    TableStructure {
        table: t("a"),
        columns: vec![c("id", "INTEGER", true), c("c", "INTEGER", false)],
        primary_key: vec!["id".into()],
        indexes: vec![IndexInfo { name: "ix_c".into(), unique: false, columns: vec!["c".into()], condition: None }],
        ..Default::default()
    }
}

#[test]
fn alter_column_sqlite_rebuilds_the_table() {
    let op = StructuralOp::AlterColumn {
        table: t("a"),
        old: col("c", "INTEGER", true, None),
        new: col("d", "TEXT", true, None),
        current: Some(Box::new(current_structure())),
    };
    assert_eq!(
        ddl(DriverId::Sqlite, op).unwrap(),
        vec![
            "CREATE TABLE \"a__new\" (\n  \"id\" INTEGER NOT NULL,\n  \"d\" TEXT,\n  PRIMARY KEY (\"id\")\n)",
            "INSERT INTO \"a__new\" (\"id\", \"d\") SELECT \"id\", \"c\" FROM \"a\"",
            "DROP TABLE \"a\"",
            "ALTER TABLE \"a__new\" RENAME TO \"a\"",
            "CREATE INDEX \"ix_c\" ON \"a\" (\"d\")",
        ]
    );
}

#[test]
fn alter_column_sqlite_without_structure_is_an_error() {
    let op = StructuralOp::AlterColumn { table: t("a"), old: col("c", "int", true, None), new: col("c", "text", true, None), current: None };
    assert!(matches!(ddl(DriverId::Sqlite, op), Err(DdlError::Unsupported(_))));
}

/// The SQLite rebuild keeps what the original text says and refuses only what it cannot keep safe.
#[test]
fn sqlite_rebuild_keeps_constraints_and_refuses_the_unsafe() {
    let alter = |st: TableStructure, new_name: &str| StructuralOp::AlterColumn { table: t("a"), old: col("c", "INTEGER", true, None), new: col(new_name, "TEXT", true, None), current: Some(Box::new(st)) };
    let with_ddl = |ddl_text: &str| {
        let mut st = current_structure();
        st.ddl = ddl_text.into();
        st
    };
    let ok = ddl(DriverId::Sqlite, alter(with_ddl("CREATE TABLE a (id INTEGER PRIMARY KEY AUTOINCREMENT, c INTEGER CHECK (c > 0), e TEXT UNIQUE COLLATE NOCASE)"), "c")).unwrap();
    assert!(ok[0].contains("\"c\" TEXT CHECK (c > 0)") && ok[0].contains("UNIQUE COLLATE NOCASE") && ok[0].contains("AUTOINCREMENT"), "{}", ok[0]);
    let refused = |st: TableStructure, name: &str| matches!(ddl(DriverId::Sqlite, alter(st, name)), Err(DdlError::Unsupported(m)) if m.contains("cannot be rebuilt safely"));
    assert!(refused(with_ddl("CREATE TABLE a (id INTEGER, c INTEGER, g INTEGER GENERATED ALWAYS AS (id + 1))"), "c"));
    let mut referenced = with_ddl("CREATE TABLE a (id INTEGER, c INTEGER)");
    referenced.referenced_by = vec!["child".into()];
    assert!(refused(referenced, "c"));
    assert!(refused(with_ddl("CREATE TABLE a (id INTEGER, c INTEGER, UNIQUE (c))"), "d"), "a rename that other definitions use");
    let mut trigger = with_ddl("CREATE TABLE a (id INTEGER, c INTEGER)");
    trigger.index_sql = vec!["CREATE INDEX ix_c ON a (c) WHERE c > 0".into()];
    assert!(refused(trigger.clone(), "d"));
    let kept = ddl(DriverId::Sqlite, alter(trigger, "c")).unwrap();
    assert_eq!(kept.last().unwrap(), "CREATE INDEX ix_c ON a (c) WHERE c > 0", "the partial index is recreated as written");
}

#[test]
fn indexes() {
    let add = || StructuralOp::AddIndex { table: t("a"), index: IndexDef { name: "ix".into(), unique: true, columns: vec!["x".into(), "y".into()] } };
    assert_eq!(ddl(DriverId::Postgres, add()).unwrap(), vec!["CREATE UNIQUE INDEX \"ix\" ON \"a\" (\"x\", \"y\")"]);
    let drop = |id| ddl(id, StructuralOp::DropIndex { table: t("a"), name: "ix".into() }).unwrap();
    assert_eq!(drop(DriverId::MySql), vec!["DROP INDEX `ix` ON `a`"]);
    assert_eq!(drop(DriverId::Sqlite), vec!["DROP INDEX \"ix\""]);
}

#[test]
fn foreign_keys_postgres_mysql_and_sqlite_refusal() {
    let fk = || ForeignKeyDef {
        name: Some("fk1".into()),
        columns: vec!["b".into()],
        ref_table: t("r"),
        ref_columns: vec!["id".into()],
        on_update: "CASCADE".into(),
        on_delete: "SET NULL".into(),
    };
    assert_eq!(
        ddl(DriverId::Postgres, StructuralOp::AddForeignKey { table: t("a"), fk: fk() }).unwrap(),
        vec!["ALTER TABLE \"a\" ADD CONSTRAINT \"fk1\" FOREIGN KEY (\"b\") REFERENCES \"r\" (\"id\") ON UPDATE CASCADE ON DELETE SET NULL"]
    );
    assert_eq!(
        ddl(DriverId::MySql, StructuralOp::DropForeignKey { table: t("a"), name: "fk1".into() }).unwrap(),
        vec!["ALTER TABLE `a` DROP FOREIGN KEY `fk1`"]
    );
    assert!(matches!(ddl(DriverId::Sqlite, StructuralOp::AddForeignKey { table: t("a"), fk: fk() }), Err(DdlError::Unsupported(_))));
}

#[test]
fn clone_table() {
    let op = |with_data| StructuralOp::CloneTable { source: t("a"), target: t("a_clone"), with_data };
    assert_eq!(
        ddl(DriverId::Postgres, op(true)).unwrap(),
        vec!["CREATE TABLE \"a_clone\" (LIKE \"a\" INCLUDING ALL)", "INSERT INTO \"a_clone\" SELECT * FROM \"a\""]
    );
    assert_eq!(ddl(DriverId::MySql, op(false)).unwrap(), vec!["CREATE TABLE `a_clone` LIKE `a`"]);
    assert_eq!(ddl(DriverId::Sqlite, op(false)).unwrap(), vec!["CREATE TABLE \"a_clone\" AS SELECT * FROM \"a\" WHERE 0"]);
}

#[test]
fn raw_passes_through() {
    assert_eq!(ddl(DriverId::Sqlite, StructuralOp::Raw("CREATE VIEW v AS SELECT 1".into())).unwrap(), vec!["CREATE VIEW v AS SELECT 1"]);
}

/// Comment and collation are repeated for text columns only, and quotes in the comment are escaped.
#[test]
fn mysql_attrs_include_comment_and_collation() {
    use std::collections::BTreeMap;
    let extra = BTreeMap::from([("collation".to_owned(), "utf8mb4_bin".to_owned()), ("comment".to_owned(), "it's a \\ test".to_owned())]);
    assert_eq!(dbzilla_core::ddl::mysql_column_attrs(&extra, "varchar(40)").as_deref(), Some("COLLATE utf8mb4_bin COMMENT 'it''s a \\\\ test'"));
    assert_eq!(dbzilla_core::ddl::mysql_column_attrs(&extra, "int").as_deref(), Some("COMMENT 'it''s a \\\\ test'"), "no collation on a number");
}
