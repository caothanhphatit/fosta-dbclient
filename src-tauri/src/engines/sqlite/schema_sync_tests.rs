//! Goldens of the SQLite schema compare dialect: normalization of the CREATE text, type affinity risks, which changes
//! need a rebuild, rendered steps (each checked against the gateway's classifier), the rebuild sequence, the wrapper
//! with the foreign key switch and check, and a round trip between two in-memory databases.

use rusqlite::Connection;

use super::{
    adapter::SQLITE,
    schema_snapshot,
    schema_sync::{REBUILD_PREFIX, SQLITE_SCHEMA_DIALECT as LITE},
};
use crate::{
    contract::{
        Category, Change, ChangeKind, CheckDef, ClassifyInput, ColumnDef, CompareOptions, DiffItem, DiffStatus, Engine, ForeignKeyDef,
        GeneratedDef, IdentityDef, IdentityKind, IndexDef, IndexPart, KeyDef, MigrationStep, RenderContext, RenderedStatement, Risk,
        SchemaDialect, SchemaSnapshot, SnapshotScope, TableDef, ViewDef,
    },
    engines::common::{classify, split::Dialect, StatementKind},
};

fn ctx() -> RenderContext {
    RenderContext {
        source_schema: Some("compare_a".into()),
        target_schema: Some("compare_b".into()),
    }
}

fn render(step: MigrationStep) -> Vec<RenderedStatement> {
    LITE.render(&step, &ctx())
}

fn one(step: MigrationStep) -> String {
    let rendered = render(step);
    assert_eq!(rendered.len(), 1, "{rendered:?}");
    rendered.into_iter().next().unwrap().sql
}

/// Every statement is read by the gateway's parser (not by its keyword fallback) and lands in the category the
/// confirmation rules expect: DDL, destructive, a transaction, the copy (write), the foreign key check (read) or the
/// foreign key PRAGMA (admin). Comment-only text (a manual step) is skipped.
fn assert_classified(sql: &str) {
    let classification = SQLITE.classify(&ClassifyInput::Statement {
        language: "sql",
        text: sql,
    });
    for info in &classification.statements {
        let pragma = info.excerpt.trim_start().to_ascii_uppercase().starts_with("PRAGMA");
        let allowed = match info.category {
            Category::Ddl | Category::Destructive | Category::Transaction | Category::Write | Category::Read => true,
            Category::Admin => pragma,
            _ => false,
        };
        assert!(allowed, "{sql} is classified as {:?} ({:?})", info.category, info.reason);
    }
    for statement in classify(Dialect::Sqlite, sql).statements {
        let pragma = statement.keyword.starts_with("PRAGMA");
        // sqlparser 0.63 reads WITHOUT ROWID and STRICT, but not both (SQLite separates them with a comma): the keyword
        // rules classify that CREATE TABLE as DDL.
        let both_options = statement.kind == StatementKind::Ddl && statement.text.contains("WITHOUT ROWID, STRICT");
        assert!(
            statement.parsed || pragma || both_options,
            "the parser cannot read {}",
            statement.text
        );
        assert_ne!(statement.kind, StatementKind::Unknown, "{}", statement.text);
    }
}

fn column(name: &str, type_name: &str, nullable: bool) -> ColumnDef {
    ColumnDef::new(name, type_name, nullable)
}

fn change(part: &str, name: &str, what: ChangeKind) -> Change {
    Change {
        part: part.into(),
        name: name.into(),
        what,
        fields: Vec::new(),
        risk: Risk::None,
        note: None,
        possible_rename: None,
    }
}

fn item(changes: Vec<Change>) -> DiffItem {
    DiffItem {
        id: "table:orders".into(),
        kind: "table".into(),
        name: "orders".into(),
        status: DiffStatus::Different,
        changes,
        risk: Risk::None,
        source_ddl: None,
        target_ddl: None,
    }
}

fn orders() -> TableDef {
    let mut id = column("id", "INTEGER", true);
    id.identity = Some(IdentityDef {
        kind: IdentityKind::AutoIncrement,
        start: None,
        increment: None,
    });
    let mut status = column("status", "VARCHAR(20)", false);
    status.default = Some("'new'".into());
    let mut code = column("code", "TEXT", true);
    code.collation = Some("NOCASE".into());
    let mut total = column("total", "REAL", true);
    total.generated = Some(GeneratedDef {
        expression: "qty * 2.5".into(),
        stored: false,
    });
    TableDef {
        columns: vec![
            id,
            column("customer_id", "INTEGER", false),
            status,
            column("qty", "INT", false),
            code,
            total,
        ],
        primary_key: Some(KeyDef {
            name: None,
            columns: vec!["id".into()],
            system_named: true,
        }),
        uniques: vec![KeyDef {
            name: Some("uq_orders_code".into()),
            columns: vec!["code".into()],
            system_named: false,
        }],
        checks: vec![CheckDef {
            name: Some("ck_qty".into()),
            expression: "qty > 0".into(),
            system_named: false,
        }],
        indexes: vec![IndexDef {
            name: "ix_orders_open".into(),
            parts: vec![
                IndexPart {
                    descending: true,
                    ..IndexPart::column("customer_id")
                },
                IndexPart {
                    column: None,
                    expression: Some("lower(code)".into()),
                    descending: false,
                },
            ],
            unique: false,
            method: None,
            predicate: Some("status <> 'done'".into()),
            include: Vec::new(),
            system_named: false,
        }],
        foreign_keys: vec![ForeignKeyDef {
            name: Some("fk_customer".into()),
            columns: vec!["customer_id".into()],
            ref_schema: None,
            ref_table: "customers".into(),
            ref_columns: vec!["id".into()],
            on_update: None,
            on_delete: Some("CASCADE".into()),
            system_named: false,
        }],
        options: vec![("strict".into(), "true".into())],
        native_ddl: Some(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY AUTOINCREMENT);\n\
             CREATE TRIGGER tr_orders AFTER INSERT ON orders BEGIN UPDATE orders SET qty = 1 WHERE id = new.id; END;"
                .into(),
        ),
        ..TableDef::new("orders")
    }
}

// --- Normalization ------------------------------------------------------------------------------------------------------

#[test]
fn types_defaults_and_expressions_compare_without_quotes_case_or_spacing() {
    let ty = |raw: &str| LITE.normalize_type(raw, Some("compare_a"));
    assert_eq!(ty("varchar ( 100 )"), "VARCHAR(100)");
    assert_eq!(ty("decimal(12, 2)"), "DECIMAL(12,2)");
    assert_eq!(ty("unsigned  big int"), "UNSIGNED BIG INT");
    assert_ne!(ty("INT"), ty("INTEGER"), "INTEGER PRIMARY KEY is the rowid alias, INT is not");
    let default = |raw: &str| LITE.normalize_default(raw, Some("compare_a"));
    assert_eq!(default("'new'"), "'new'");
    assert_eq!(default("(1 + 1)"), "1+1");
    assert_eq!(default("CURRENT_TIMESTAMP"), default("current_timestamp"));
    assert_eq!(default("NULL"), "");
    assert_eq!(default("'A'"), "'A'", "literals keep their case");
    let expr = |raw: &str| LITE.normalize_expression(raw, Some("compare_a"));
    assert_eq!(expr("(\"qty\" > 0)"), expr("qty>0"));
    assert_eq!(expr("length( [what] ) > 0 -- note"), "length(what)>0");
    assert_eq!(
        expr("SELECT o.id FROM compare_a.orders o"),
        LITE.normalize_expression("select o.id from orders o", Some("compare_b"))
    );
}

#[test]
fn type_changes_are_rated_by_affinity() {
    let risk = |from: &str, to: &str| LITE.type_change_risk(&column("c", from, true), &column("c", to, true));
    assert_eq!(risk("VARCHAR(20)", "varchar(10)"), Risk::None, "SQLite does not enforce lengths");
    assert_eq!(risk("INT", "BIGINT"), Risk::None);
    assert_eq!(risk("TEXT", "INTEGER"), Risk::Review);
    assert_eq!(risk("DECIMAL(12,2)", "INTEGER"), Risk::Review);
    assert_eq!(risk("REAL", "DOUBLE"), Risk::None);
}

#[test]
fn only_index_changes_and_plain_column_adds_and_drops_avoid_a_rebuild() {
    let source = orders();
    let mut target = orders();
    let needs = |changes: Vec<Change>, source: &TableDef, target: &TableDef| LITE.needs_rebuild(&item(changes), source, target);
    assert!(!needs(
        vec![change("index", "ix_orders_open", ChangeKind::Modified)],
        &source,
        &target
    ));
    assert!(needs(vec![change("column", "status", ChangeKind::Modified)], &source, &target));
    assert!(needs(vec![change("check", "ck_qty", ChangeKind::Modified)], &source, &target));
    assert!(needs(
        vec![change("foreignKey", "fk_customer", ChangeKind::Added)],
        &source,
        &target
    ));
    assert!(needs(vec![change("option", "strict", ChangeKind::Added)], &source, &target));
    assert!(needs(vec![change("columnOrder", "orders", ChangeKind::Modified)], &source, &target));

    // Added: nullable, a constant default or a virtual generated column is fine; NOT NULL without a default, a
    // CURRENT_TIMESTAMP default, a stored generated column or a key column is not.
    let with = |column: ColumnDef| {
        let mut table = orders();
        let name = column.name.clone();
        table.columns.push(column);
        LITE.needs_rebuild(&item(vec![change("column", &name, ChangeKind::Added)]), &table, &target)
    };
    assert!(!with(column("note", "TEXT", true)));
    assert!(!with(ColumnDef {
        default: Some("0".into()),
        ..column("n", "INT", false)
    }));
    assert!(with(column("n", "INT", false)));
    assert!(with(ColumnDef {
        default: Some("CURRENT_TIMESTAMP".into()),
        ..column("at", "TEXT", true)
    }));
    assert!(with(ColumnDef {
        generated: Some(GeneratedDef {
            expression: "1".into(),
            stored: true
        }),
        ..column("g", "INT", true)
    }));
    assert!(!with(ColumnDef {
        generated: Some(GeneratedDef {
            expression: "qty + 1".into(),
            stored: false
        }),
        ..column("g", "INT", true)
    }));
    let mut keyed = orders();
    keyed.columns.push(column("ref", "INT", true));
    keyed.uniques.push(KeyDef {
        name: None,
        columns: vec!["ref".into()],
        system_named: true,
    });
    assert!(needs(vec![change("column", "ref", ChangeKind::Added)], &keyed, &target));

    // Removed: a column no key, check or generated column uses drops in place.
    target.columns.push(column("old", "TEXT", true));
    assert!(!needs(vec![change("column", "old", ChangeKind::Removed)], &source, &target));
    assert!(
        needs(vec![change("column", "qty", ChangeKind::Removed)], &source, &target),
        "a check uses it"
    );
    assert!(
        needs(vec![change("column", "code", ChangeKind::Removed)], &source, &target),
        "a unique key uses it"
    );
    assert!(
        needs(vec![change("column", "customer_id", ChangeKind::Removed)], &source, &target),
        "a foreign key uses it"
    );
}

// --- Rendering ----------------------------------------------------------------------------------------------------------

#[test]
fn tables_are_created_with_keys_checks_foreign_keys_and_options() {
    let sql = one(MigrationStep::CreateTable { table: orders() });
    assert_eq!(
        sql,
        "CREATE TABLE \"compare_b\".\"orders\" (\n    \
         \"id\" INTEGER PRIMARY KEY AUTOINCREMENT,\n    \
         \"customer_id\" INTEGER NOT NULL,\n    \
         \"status\" VARCHAR(20) NOT NULL DEFAULT 'new',\n    \
         \"qty\" INT NOT NULL,\n    \
         \"code\" TEXT COLLATE \"NOCASE\",\n    \
         \"total\" REAL GENERATED ALWAYS AS (qty * 2.5) VIRTUAL,\n    \
         CONSTRAINT \"uq_orders_code\" UNIQUE (\"code\"),\n    \
         CONSTRAINT \"ck_qty\" CHECK (qty > 0),\n    \
         CONSTRAINT \"fk_customer\" FOREIGN KEY (\"customer_id\") REFERENCES \"customers\" (\"id\") ON DELETE CASCADE\n\
         ) STRICT"
    );
    assert_classified(&sql);

    let mut plain = orders();
    plain.columns[0].identity = None;
    plain.options = vec![("withoutRowid".into(), "true".into()), ("strict".into(), "true".into())];
    plain.primary_key.as_mut().unwrap().name = Some("pk_orders".into());
    plain.primary_key.as_mut().unwrap().system_named = false;
    let sql = one(MigrationStep::CreateTable { table: plain });
    assert!(sql.contains("CONSTRAINT \"pk_orders\" PRIMARY KEY (\"id\")"), "{sql}");
    assert!(sql.ends_with(") WITHOUT ROWID, STRICT"), "{sql}");
    assert_classified(&sql);

    let ddl = LITE.table_ddl(Some("compare_b"), &orders());
    assert!(ddl.ends_with(
        "CREATE INDEX \"compare_b\".\"ix_orders_open\" ON \"orders\" (\"customer_id\" DESC, lower(code)) WHERE status <> 'done';"
    ));
}

#[test]
fn columns_indexes_and_views_render_as_alter_and_create_statements() {
    let mut note = column("note", "TEXT", false);
    note.default = Some("''".into());
    let statements = [
        one(MigrationStep::AddColumn {
            table: "orders".into(),
            column: note,
        }),
        one(MigrationStep::DropColumn {
            table: "orders".into(),
            column: "old".into(),
        }),
        one(MigrationStep::DropTable { name: "legacy".into() }),
        one(MigrationStep::DropIndex {
            table: "orders".into(),
            name: "ix_orders_open".into(),
        }),
        one(MigrationStep::CreateIndex {
            table: "orders".into(),
            index: orders().indexes[0].clone(),
        }),
        one(MigrationStep::DropView {
            name: "v_orders".into(),
            materialized: false,
        }),
        one(MigrationStep::CreateView {
            view: ViewDef {
                name: "v_orders".into(),
                definition: "SELECT o.id FROM orders o WHERE o.qty > 0".into(),
                ..ViewDef::default()
            },
            replace: false,
        }),
    ];
    assert_eq!(
        statements,
        [
            "ALTER TABLE \"compare_b\".\"orders\" ADD COLUMN \"note\" TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE \"compare_b\".\"orders\" DROP COLUMN \"old\"",
            "DROP TABLE \"compare_b\".\"legacy\"",
            "DROP INDEX \"compare_b\".\"ix_orders_open\"",
            "CREATE INDEX \"compare_b\".\"ix_orders_open\" ON \"orders\" (\"customer_id\" DESC, lower(code)) WHERE status <> 'done'",
            "DROP VIEW \"compare_b\".\"v_orders\"",
            "CREATE VIEW \"compare_b\".\"v_orders\" AS\nSELECT o.id FROM orders o WHERE o.qty > 0",
        ]
    );
    for sql in &statements {
        assert_classified(sql);
    }
    // Defaults that are expressions need parentheses.
    let mut at = column("at", "TEXT", true);
    at.default = Some("datetime('now')".into());
    let sql = one(MigrationStep::CreateTable {
        table: TableDef {
            columns: vec![at],
            ..TableDef::new("t")
        },
    });
    assert!(sql.contains("\"at\" TEXT DEFAULT (datetime('now'))"), "{sql}");
}

#[test]
fn identifiers_with_quotes_spaces_and_reserved_words_are_quoted() {
    let sql = one(MigrationStep::DropColumn {
        table: "order \"items\"".into(),
        column: "select".into(),
    });
    assert_eq!(sql, "ALTER TABLE \"compare_b\".\"order \"\"items\"\"\" DROP COLUMN \"select\"");
    assert_classified(&sql);
}

#[test]
fn a_rebuild_copies_common_columns_and_recreates_indexes_and_triggers() {
    let source = orders();
    let mut target = orders();
    target.columns.retain(|c| c.name != "code");
    target.columns.push(column("legacy", "TEXT", true));
    let column_map: Vec<(String, String)> = ["id", "customer_id", "status", "qty", "total"]
        .iter()
        .map(|c| ((*c).to_owned(), (*c).to_owned()))
        .collect();
    let rendered = render(MigrationStep::RebuildTable {
        source_table: source,
        target_table: target,
        column_map,
    });
    let sql: Vec<&str> = rendered.iter().map(|s| s.sql.as_str()).collect();
    let new = format!("\"compare_b\".\"{REBUILD_PREFIX}orders\"");
    assert!(sql[0].starts_with(&format!("CREATE TABLE {new} (")), "{}", sql[0]);
    assert_eq!(
        sql[1..],
        [
            format!("INSERT INTO {new} (\"id\", \"customer_id\", \"status\", \"qty\") SELECT \"id\", \"customer_id\", \"status\", \"qty\" FROM \"compare_b\".\"orders\""),
            "DROP TABLE \"compare_b\".\"orders\"".to_owned(),
            format!("ALTER TABLE {new} RENAME TO \"orders\""),
            "CREATE INDEX \"compare_b\".\"ix_orders_open\" ON \"orders\" (\"customer_id\" DESC, lower(code)) WHERE status <> 'done'".to_owned(),
            "CREATE TRIGGER \"compare_b\".\"tr_orders\" AFTER INSERT ON orders BEGIN UPDATE orders SET qty = 1 WHERE id = new.id; END".to_owned(),
        ]
    );
    assert_eq!(rendered[0].risk, Risk::DataLoss, "the target's legacy column is dropped");
    assert!(
        rendered[0].warnings.iter().any(|w| w.contains("legacy")),
        "{:?}",
        rendered[0].warnings
    );
    assert!(rendered[5].warnings[0].contains("tr_orders"));
    for statement in &rendered {
        assert_classified(&statement.sql);
    }
}

#[test]
fn changes_sqlite_cannot_make_in_place_are_manual_steps() {
    for step in [
        MigrationStep::AlterColumn {
            table: "orders".into(),
            from: column("qty", "INT", true),
            to: column("qty", "BIGINT", true),
            changes: Default::default(),
        },
        MigrationStep::DropForeignKey {
            table: "orders".into(),
            name: "fk".into(),
        },
        MigrationStep::SetTableOption {
            table: "orders".into(),
            name: "strict".into(),
            value: "true".into(),
        },
    ] {
        let sql = one(step);
        assert!(sql.starts_with("-- MANUAL STEP:") && sql.contains("rebuild"), "{sql}");
    }
}

#[test]
fn the_wrapper_switches_foreign_keys_off_around_drops_and_checks_them_before_commit() {
    let body = vec![
        RenderedStatement::new("DROP TABLE \"compare_b\".\"legacy\""),
        RenderedStatement::new("CREATE INDEX \"compare_b\".\"ix\" ON \"t\" (\"a\")"),
    ];
    let wrapped = LITE.wrap(body.clone(), true);
    let sql: Vec<&str> = wrapped.iter().map(|s| s.sql.as_str()).collect();
    assert_eq!(
        sql,
        [
            "PRAGMA foreign_keys = OFF",
            "BEGIN",
            "DROP TABLE \"compare_b\".\"legacy\"",
            "CREATE INDEX \"compare_b\".\"ix\" ON \"t\" (\"a\")",
            "SELECT CASE WHEN EXISTS (SELECT 1 FROM pragma_foreign_key_check(NULL, 'compare_b')) THEN abs(-9223372036854775808) END",
            "COMMIT",
            "PRAGMA foreign_keys = ON",
        ]
    );
    for statement in &wrapped {
        assert_classified(&statement.sql);
    }
    let admin = SQLITE.classify(&ClassifyInput::Statement {
        language: "sql",
        text: "PRAGMA foreign_keys = OFF",
    });
    assert_eq!(admin.worst(), Some(Category::Admin));
    // Without drops or rebuilds: no PRAGMA, so no admin confirmation.
    let plain = LITE.wrap(body[1..].to_vec(), true);
    assert_eq!(
        plain.iter().map(|s| s.sql.as_str()).collect::<Vec<_>>(),
        ["BEGIN", body[1].sql.as_str(), "COMMIT"]
    );
    assert_eq!(LITE.wrap(body[1..].to_vec(), false).len(), 1);
}

// --- Round trip ---------------------------------------------------------------------------------------------------------

const SOURCE: &str = "
CREATE TABLE a.customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL COLLATE NOCASE, CONSTRAINT uq_email UNIQUE (email));
CREATE TABLE a.orders (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  customer_id INTEGER NOT NULL REFERENCES customers (id) ON DELETE CASCADE,
  status VARCHAR(20) NOT NULL DEFAULT 'new',
  qty INTEGER NOT NULL DEFAULT 1 CHECK (qty > 0),
  total REAL GENERATED ALWAYS AS (qty * 2.5) VIRTUAL,
  note TEXT
);
CREATE INDEX a.ix_orders_status ON orders (status DESC) WHERE status <> 'done';
CREATE TABLE a.items (id INTEGER PRIMARY KEY, order_id INTEGER NOT NULL REFERENCES orders (id), sku TEXT NOT NULL, gift TEXT);
CREATE INDEX a.ix_items_sku ON items (lower(sku));
CREATE TABLE a.created (k TEXT PRIMARY KEY, v BLOB) WITHOUT ROWID;
CREATE VIEW a.v_orders AS SELECT o.id, c.email FROM orders o JOIN customers c ON c.id = o.customer_id;
";

const TARGET: &str = "
CREATE TABLE b.customers (id INTEGER PRIMARY KEY, email TEXT NOT NULL COLLATE NOCASE, CONSTRAINT uq_email UNIQUE (email));
CREATE TABLE b.orders (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  customer_id INTEGER NOT NULL REFERENCES customers (id),
  status VARCHAR(10),
  qty INT NOT NULL DEFAULT 1 CHECK (qty >= 0),
  legacy TEXT
);
CREATE TRIGGER b.tr_orders AFTER UPDATE OF status ON orders BEGIN UPDATE orders SET legacy = new.status WHERE id = new.id; END;
CREATE TABLE b.items (id INTEGER PRIMARY KEY, order_id INTEGER NOT NULL REFERENCES orders (id), sku TEXT NOT NULL, obsolete TEXT);
CREATE TABLE b.dropped (id INTEGER PRIMARY KEY);
CREATE VIEW b.v_orders AS SELECT o.id, c.email FROM orders o JOIN customers c ON c.id = o.customer_id;
INSERT INTO b.customers VALUES (1, 'a@example.com');
INSERT INTO b.orders (id, customer_id, status, qty) VALUES (1, 1, 'open', 2);
INSERT INTO b.items VALUES (1, 1, 'X', NULL);
";

fn two_databases() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("PRAGMA foreign_keys = ON; ATTACH ':memory:' AS a; ATTACH ':memory:' AS b;")
        .unwrap();
    c.execute_batch(SOURCE).unwrap();
    c.execute_batch(TARGET).unwrap();
    c
}

fn snap(c: &Connection, scope: &str) -> SchemaSnapshot {
    schema_snapshot::snapshot(c, &SnapshotScope::new(scope, None), "3").unwrap()
}

#[test]
fn a_script_from_two_databases_rebuilds_and_leaves_nothing_to_sync() {
    let c = two_databases();
    let (source, target) = (snap(&c, "a"), snap(&c, "b"));
    let options = CompareOptions::default();
    let diff = crate::compare::diff(&source, &target, &options, &LITE);
    let statuses: Vec<(&str, DiffStatus)> = diff.items.iter().map(|i| (i.id.as_str(), i.status)).collect();
    assert_eq!(
        statuses,
        [
            ("table:created", DiffStatus::OnlyInSource),
            ("table:customers", DiffStatus::Identical),
            ("table:dropped", DiffStatus::OnlyInTarget),
            ("table:items", DiffStatus::Different),
            ("table:orders", DiffStatus::Different),
            ("view:v_orders", DiffStatus::Identical),
        ]
    );
    let include: Vec<String> = diff.items.iter().map(|i| i.id.clone()).collect();
    let script = crate::compare::sync_script(&source, &target, &options, &include, &LITE, "now");
    assert!(script.transactional);
    assert_classified(&script.sql);
    let sql = &script.sql;
    // items changes in place; orders is rebuilt with its index and the target's trigger; the view around it.
    for expected in [
        "ALTER TABLE \"b\".\"items\" ADD COLUMN \"gift\" TEXT;",
        "ALTER TABLE \"b\".\"items\" DROP COLUMN \"obsolete\";",
        "CREATE INDEX \"b\".\"ix_items_sku\" ON \"items\" (lower(sku));",
        "ALTER TABLE \"b\".\"_fosta_new_orders\" RENAME TO \"orders\";",
        "CREATE TRIGGER \"b\".\"tr_orders\"",
        "DROP VIEW \"b\".\"v_orders\";",
    ] {
        assert!(sql.contains(expected), "missing {expected}:\n{sql}");
    }
    let position = |text: &str| sql.find(text).unwrap_or_else(|| panic!("{text}:\n{sql}"));
    assert!(position("PRAGMA foreign_keys = OFF;") < position("BEGIN;"));
    assert!(position("DROP VIEW") < position("DROP TABLE \"b\".\"orders\""));
    assert!(position("RENAME TO \"orders\"") < position("CREATE VIEW"));
    assert!(position("pragma_foreign_key_check") < position("COMMIT;"));

    c.execute_batch(sql).unwrap_or_else(|error| panic!("{error}\n{sql}"));
    let synced = snap(&c, "b");
    let again = crate::compare::diff(&source, &synced, &options, &LITE);
    let left: Vec<_> = again.items.iter().filter(|i| i.status != DiffStatus::Identical).collect();
    assert!(left.is_empty(), "{left:#?}\n{sql}");
    assert_eq!(synced.skipped, target.skipped, "the trigger is back");
    // The rows survived the rebuild, the foreign keys hold, and enforcement is on again.
    let rows: i64 = c
        .query_row("SELECT count(*) FROM b.orders WHERE status = 'open'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    let broken: i64 = c
        .query_row("SELECT count(*) FROM pragma_foreign_key_check(NULL, 'b')", [], |r| r.get(0))
        .unwrap();
    assert_eq!(broken, 0);
    let enforced: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
    assert_eq!(enforced, 1);
}

#[test]
fn a_rebuild_that_breaks_a_foreign_key_stops_before_commit() {
    let c = two_databases();
    // An item pointing at an order that does not exist: invisible while keys are off, caught by the check.
    c.execute_batch("PRAGMA foreign_keys = OFF; INSERT INTO b.items VALUES (2, 99, 'Y', NULL); PRAGMA foreign_keys = ON;")
        .unwrap();
    let (source, target) = (snap(&c, "a"), snap(&c, "b"));
    let options = CompareOptions::default();
    let script = crate::compare::sync_script(&source, &target, &options, &["table:orders".to_owned()], &LITE, "now");
    let error = c.execute_batch(&script.sql).unwrap_err();
    assert!(error.to_string().contains("integer overflow"), "{error}");
    c.execute_batch("ROLLBACK; PRAGMA foreign_keys = ON;").unwrap();
    let still: String = c
        .query_row("SELECT sql FROM b.sqlite_schema WHERE name = 'orders'", [], |r| r.get(0))
        .unwrap();
    assert!(still.contains("legacy"), "the old table is back: {still}");
}
