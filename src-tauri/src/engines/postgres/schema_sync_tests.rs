//! Goldens of the PostgreSQL schema compare dialect: normalization, type change risks, rendered steps (each checked
//! against the gateway's classifier), the transaction wrapper and a whole script from two snapshots.

use super::{adapter::POSTGRES, schema_snapshot::check_expression, schema_sync::PG_SCHEMA_DIALECT as PG};
use crate::contract::{
    Category, CheckDef, ClassifyInput, ColumnChanges, ColumnDef, CommentTarget, CompareOptions, ConstraintDef, ConstraintKind, Engine,
    ForeignKeyDef, GeneratedDef, IdentityDef, IdentityKind, IndexDef, IndexPart, KeyDef, MigrationStep, RenderContext, Risk, SchemaDialect,
    SchemaSnapshot, SnapshotScope, TableDef, ViewDef,
};

fn ctx() -> RenderContext {
    RenderContext {
        source_schema: Some("compare_a".into()),
        target_schema: Some("compare_b".into()),
    }
}

fn render(step: MigrationStep) -> Vec<String> {
    PG.render(&step, &ctx()).into_iter().map(|s| s.sql).collect()
}

fn one(step: MigrationStep) -> String {
    let rendered = render(step);
    assert_eq!(rendered.len(), 1, "{rendered:?}");
    rendered.into_iter().next().unwrap()
}

fn assert_classified(sql: &str) {
    let classification = POSTGRES.classify(&ClassifyInput::Statement {
        language: "sql",
        text: sql,
    });
    for info in &classification.statements {
        assert!(
            matches!(info.category, Category::Ddl | Category::Destructive | Category::Transaction),
            "{sql} is {:?}",
            info.category
        );
    }
    assert!(!classification.statements.is_empty(), "{sql}");
}

fn column(name: &str, type_name: &str, nullable: bool) -> ColumnDef {
    ColumnDef::new(name, type_name, nullable)
}

fn identity(kind: IdentityKind) -> Option<IdentityDef> {
    Some(IdentityDef {
        kind,
        start: Some(1),
        increment: Some(1),
    })
}

// --- Normalization ------------------------------------------------------------------------------------------------------

#[test]
fn types_are_lowercased_collapsed_and_unqualified() {
    let a = Some("compare_a");
    assert_eq!(PG.normalize_type("character varying(50)", a), "character varying(50)");
    assert_eq!(PG.normalize_type("CHARACTER   VARYING(50)", a), "character varying(50)");
    assert_eq!(PG.normalize_type("compare_a.mood", a), "mood");
    assert_eq!(PG.normalize_type("compare_a.mood[]", a), "mood[]");
    assert_eq!(PG.normalize_type("other.mood", a), "other.mood");
    assert_eq!(
        PG.normalize_type("timestamp(3) without time zone", None),
        "timestamp(3) without time zone"
    );
}

#[test]
fn defaults_lose_literal_casts_and_the_scope_in_regclass_literals() {
    let (a, b) = (Some("compare_a"), Some("compare_b"));
    assert_eq!(PG.normalize_default("'new'::character varying", a), "'new'");
    assert_eq!(PG.normalize_default("'new'::text", b), "'new'");
    assert_eq!(PG.normalize_default("'{}'::text[]", a), "'{}'");
    assert_eq!(PG.normalize_default("'it''s'::text", a), "'it''s'");
    assert_eq!(PG.normalize_default("now()", a), "now()");
    assert_eq!(PG.normalize_default("0", a), "0");
    assert_eq!(
        PG.normalize_default("nextval('compare_a.orders_id_seq'::regclass)", a),
        PG.normalize_default("nextval('compare_b.orders_id_seq'::regclass)", b)
    );
    assert_eq!(
        PG.normalize_default("nextval('compare_a.orders_id_seq'::regclass)", a),
        "nextval('orders_id_seq'::regclass)"
    );
    assert_eq!(PG.normalize_default("'compare_a.x'::text", a), "'compare_a.x'");
    assert_eq!(PG.normalize_default("('a'::text || 'b'::text)", a), "('a'::text || 'b'::text)");
}

#[test]
fn expressions_collapse_whitespace_outside_literals_and_drop_the_scope() {
    let a = Some("compare_a");
    let body = " SELECT o.id,\n    o.total\n   FROM compare_a.orders o\n  WHERE o.note = 'a  b';";
    assert_eq!(
        PG.normalize_expression(body, a),
        "SELECT o.id, o.total FROM orders o WHERE o.note = 'a  b'"
    );
    assert_eq!(PG.normalize_expression("(qty > 0)", a), "(qty > 0)");
}

#[test]
fn check_definitions_keep_their_expression() {
    assert_eq!(check_expression("CHECK ((qty > 0))"), "(qty > 0)");
    assert_eq!(check_expression("CHECK (qty > 0) NOT VALID"), "qty > 0");
    assert_eq!(check_expression("CHECK ((a > 0) AND (b > 0))"), "(a > 0) AND (b > 0)");
    assert_eq!(check_expression("CHECK (((a)::text <> ')'::text))"), "((a)::text <> ')'::text)");
}

// --- Type change risks --------------------------------------------------------------------------------------------------

#[test]
fn type_change_risks_follow_widening_and_narrowing() {
    let risk = |from: &str, to: &str| PG.type_change_risk(&column("c", from, true), &column("c", to, true));
    let cases = [
        ("integer", "bigint", Risk::None),
        ("bigint", "integer", Risk::DataLoss),
        ("smallint", "integer", Risk::None),
        ("character varying(20)", "character varying(50)", Risk::None),
        ("character varying(50)", "character varying(20)", Risk::DataLoss),
        ("character varying(50)", "text", Risk::None),
        ("text", "character varying(50)", Risk::DataLoss),
        ("character varying(50)", "character varying", Risk::None),
        ("numeric(12,2)", "numeric(14,2)", Risk::None),
        ("numeric(12,2)", "numeric(12,1)", Risk::DataLoss),
        ("numeric(12,2)", "numeric(10,2)", Risk::DataLoss),
        ("numeric(12,2)", "numeric", Risk::None),
        ("integer", "numeric(12,2)", Risk::None),
        ("bigint", "numeric(12,2)", Risk::DataLoss),
        ("numeric(12,2)", "integer", Risk::DataLoss),
        ("double precision", "real", Risk::DataLoss),
        ("integer", "double precision", Risk::Review),
        ("timestamp(6) without time zone", "timestamp(3) without time zone", Risk::DataLoss),
        ("timestamp without time zone", "timestamp(3) without time zone", Risk::DataLoss),
        ("integer", "text", Risk::None),
        ("text", "integer", Risk::Review),
        ("integer", "integer[]", Risk::Review),
        ("integer", "integer", Risk::None),
    ];
    for (from, to, expected) in cases {
        assert_eq!(risk(from, to), expected, "{from} -> {to}");
    }
}

// --- Rendering ----------------------------------------------------------------------------------------------------------

fn orders() -> TableDef {
    let mut id = column("id", "integer", false);
    id.identity = identity(IdentityKind::ByDefault);
    let mut status = column("status", "character varying(20)", false);
    status.default = Some("'new'::character varying".into());
    let mut code = column("code", "text", true);
    code.collation = Some("C".into());
    let mut total = column("total", "numeric(12,2)", true);
    total.generated = Some(GeneratedDef {
        expression: "(qty * 2)".into(),
        stored: true,
    });
    TableDef {
        columns: vec![id, column("qty", "integer", false), status, code, total],
        primary_key: Some(KeyDef {
            name: Some("orders_pkey".into()),
            columns: vec!["id".into()],
            system_named: false,
        }),
        uniques: vec![KeyDef {
            name: Some("orders_code_key".into()),
            columns: vec!["code".into()],
            system_named: false,
        }],
        checks: vec![CheckDef {
            name: Some("orders_qty_check".into()),
            expression: "(qty > 0)".into(),
            system_named: false,
        }],
        comment: Some("All orders".into()),
        ..TableDef::new("orders")
    }
}

#[test]
fn create_table_renders_columns_keys_and_checks() {
    let sql = one(MigrationStep::CreateTable { table: orders() });
    assert_eq!(
        sql,
        "CREATE TABLE compare_b.orders (\n    \
         id integer GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n    \
         qty integer NOT NULL,\n    \
         status character varying(20) DEFAULT 'new'::character varying NOT NULL,\n    \
         code text COLLATE \"C\",\n    \
         total numeric(12,2) GENERATED ALWAYS AS ((qty * 2)) STORED,\n    \
         CONSTRAINT orders_pkey PRIMARY KEY (id),\n    \
         CONSTRAINT orders_code_key UNIQUE (code),\n    \
         CONSTRAINT orders_qty_check CHECK ((qty > 0))\n)"
    );
    assert_classified(&sql);
}

#[test]
fn serial_columns_render_as_serial_types_and_map_their_sequences() {
    let mut id = column("id", "bigint", false);
    id.identity = Some(IdentityDef {
        kind: IdentityKind::Serial,
        start: None,
        increment: None,
    });
    id.default = Some("nextval('compare_a.t_id_seq'::regclass)".into());
    let mut seq = column("n", "integer", true);
    seq.default = Some("nextval('compare_a.shared_seq'::regclass)".into());
    let table = TableDef {
        columns: vec![id, seq],
        ..TableDef::new("t")
    };
    assert_eq!(
        one(MigrationStep::CreateTable { table }),
        "CREATE TABLE compare_b.t (\n    id bigserial NOT NULL,\n    n integer DEFAULT nextval('compare_b.shared_seq'::regclass)\n)"
    );
}

#[test]
fn table_ddl_shows_indexes_foreign_keys_and_comments() {
    let mut table = orders();
    table.indexes.push(IndexDef {
        name: "orders_lower_code_idx".into(),
        parts: vec![
            IndexPart {
                expression: Some("lower(code)".into()),
                ..IndexPart::default()
            },
            IndexPart {
                column: Some("qty".into()),
                descending: true,
                ..IndexPart::default()
            },
        ],
        unique: false,
        method: Some("btree".into()),
        predicate: Some("(qty > 10)".into()),
        include: vec!["status".into()],
        system_named: false,
    });
    table.foreign_keys.push(ForeignKeyDef {
        name: Some("orders_customer_fkey".into()),
        columns: vec!["qty".into()],
        ref_schema: None,
        ref_table: "customers".into(),
        ref_columns: vec!["id".into()],
        on_update: None,
        on_delete: Some("CASCADE".into()),
        system_named: false,
    });
    table.columns[1].comment = Some("How many".into());
    let ddl = PG.table_ddl(Some("compare_b"), &table);
    assert!(ddl.contains("    CONSTRAINT orders_customer_fkey FOREIGN KEY (qty) REFERENCES compare_b.customers (id) ON DELETE CASCADE\n);"));
    assert!(ddl.contains(
        "\n\nCREATE INDEX orders_lower_code_idx ON compare_b.orders ((lower(code)), qty DESC) INCLUDE (status) WHERE (qty > 10);"
    ));
    assert!(ddl.contains("\n\nCOMMENT ON TABLE compare_b.orders IS 'All orders';"));
    assert!(ddl.ends_with("\n\nCOMMENT ON COLUMN compare_b.orders.qty IS 'How many';"), "{ddl}");
}

#[test]
fn identifiers_with_capitals_spaces_quotes_and_reserved_words_are_quoted() {
    let table = TableDef {
        columns: vec![
            column("Order Id", "integer", false),
            column("user", "text", true),
            column("say \"hi\"", "text", true),
        ],
        ..TableDef::new("select")
    };
    let sql = one(MigrationStep::CreateTable { table });
    assert_eq!(
        sql,
        "CREATE TABLE compare_b.\"select\" (\n    \"Order Id\" integer NOT NULL,\n    \"user\" text,\n    \"say \"\"hi\"\"\" text\n)"
    );
    assert_classified(&sql);
    assert_eq!(PG.quote_ident("orders"), "orders");
    assert_eq!(PG.quote_ident("Orders"), "\"Orders\"");
}

#[test]
fn drops_render_with_data_loss_warnings() {
    let drop = PG.render(&MigrationStep::DropTable { name: "old".into() }, &ctx());
    assert_eq!(drop[0].sql, "DROP TABLE compare_b.old");
    assert_eq!(drop[0].risk, Risk::DataLoss);
    let column = PG.render(
        &MigrationStep::DropColumn {
            table: "orders".into(),
            column: "legacy".into(),
        },
        &ctx(),
    );
    assert_eq!(column[0].sql, "ALTER TABLE compare_b.orders DROP COLUMN legacy");
    assert_eq!(column[0].risk, Risk::DataLoss);
    let goldens = [
        (
            MigrationStep::DropForeignKey {
                table: "orders".into(),
                name: "orders_customer_fkey".into(),
            },
            "ALTER TABLE compare_b.orders DROP CONSTRAINT orders_customer_fkey",
        ),
        (
            MigrationStep::DropView {
                name: "v_totals".into(),
                materialized: false,
            },
            "DROP VIEW compare_b.v_totals",
        ),
        (
            MigrationStep::DropView {
                name: "mv".into(),
                materialized: true,
            },
            "DROP MATERIALIZED VIEW compare_b.mv",
        ),
        (
            MigrationStep::DropIndex {
                table: "orders".into(),
                name: "orders_note_idx".into(),
            },
            "DROP INDEX compare_b.orders_note_idx",
        ),
        (
            MigrationStep::DropConstraint {
                table: "orders".into(),
                kind: ConstraintKind::Check,
                name: Some("orders_qty_check".into()),
            },
            "ALTER TABLE compare_b.orders DROP CONSTRAINT orders_qty_check",
        ),
    ];
    for (step, expected) in goldens {
        let sql = one(step);
        assert_eq!(sql, expected);
        assert_classified(&sql);
    }
    assert_classified(&drop[0].sql);
    assert_classified(&column[0].sql);
}

#[test]
fn adds_render_constraints_indexes_and_foreign_keys() {
    let goldens = [
        (
            MigrationStep::AddColumn {
                table: "orders".into(),
                column: column("note", "text", true),
            },
            "ALTER TABLE compare_b.orders ADD COLUMN note text",
        ),
        (
            MigrationStep::AddPrimaryKey {
                table: "orders".into(),
                key: KeyDef {
                    name: Some("orders_pkey".into()),
                    columns: vec!["id".into(), "qty".into()],
                    system_named: false,
                },
            },
            "ALTER TABLE compare_b.orders ADD CONSTRAINT orders_pkey PRIMARY KEY (id, qty)",
        ),
        (
            MigrationStep::AddConstraint {
                table: "orders".into(),
                constraint: ConstraintDef::Unique {
                    key: KeyDef {
                        name: Some("orders_code_key".into()),
                        columns: vec!["code".into()],
                        system_named: false,
                    },
                },
            },
            "ALTER TABLE compare_b.orders ADD CONSTRAINT orders_code_key UNIQUE (code)",
        ),
        (
            MigrationStep::AddConstraint {
                table: "orders".into(),
                constraint: ConstraintDef::Check {
                    check: CheckDef {
                        name: Some("orders_kind_check".into()),
                        expression: "kind = ANY (ARRAY['a'::text, 'b'::text])".into(),
                        system_named: false,
                    },
                },
            },
            "ALTER TABLE compare_b.orders ADD CONSTRAINT orders_kind_check CHECK (kind = ANY (ARRAY['a'::text, 'b'::text]))",
        ),
        (
            MigrationStep::CreateIndex {
                table: "orders".into(),
                index: IndexDef {
                    name: "orders_tags_idx".into(),
                    parts: vec![IndexPart::column("tags")],
                    unique: false,
                    method: Some("gin".into()),
                    predicate: None,
                    include: Vec::new(),
                    system_named: false,
                },
            },
            "CREATE INDEX orders_tags_idx ON compare_b.orders USING gin (tags)",
        ),
        (
            MigrationStep::CreateIndex {
                table: "orders".into(),
                index: IndexDef {
                    name: "orders_open_idx".into(),
                    parts: vec![IndexPart::column("id")],
                    unique: true,
                    method: Some("btree".into()),
                    predicate: Some("(status)::text = 'open'::text".into()),
                    include: Vec::new(),
                    system_named: false,
                },
            },
            "CREATE UNIQUE INDEX orders_open_idx ON compare_b.orders (id) WHERE (status)::text = 'open'::text",
        ),
        (
            MigrationStep::AddForeignKey {
                table: "orders".into(),
                key: ForeignKeyDef {
                    name: Some("orders_customer_fkey".into()),
                    columns: vec!["customer_id".into()],
                    ref_schema: Some("compare_a".into()),
                    ref_table: "customers".into(),
                    ref_columns: vec!["id".into()],
                    on_update: Some("NO ACTION".into()),
                    on_delete: Some("SET NULL".into()),
                    system_named: false,
                },
            },
            "ALTER TABLE compare_b.orders ADD CONSTRAINT orders_customer_fkey FOREIGN KEY (customer_id) REFERENCES compare_b.customers (id) ON DELETE SET NULL",
        ),
        (
            MigrationStep::SetComment {
                target: CommentTarget {
                    kind: "table".into(),
                    name: "orders".into(),
                    column: Some("qty".into()),
                    definition: None,
                },
                comment: Some("It's many".into()),
            },
            "COMMENT ON COLUMN compare_b.orders.qty IS 'It''s many'",
        ),
        (
            MigrationStep::SetComment {
                target: CommentTarget {
                    kind: "materializedView".into(),
                    name: "mv".into(),
                    column: None,
                    definition: None,
                },
                comment: None,
            },
            "COMMENT ON MATERIALIZED VIEW compare_b.mv IS NULL",
        ),
    ];
    for (step, expected) in goldens {
        let sql = one(step);
        assert_eq!(sql, expected);
        assert_classified(&sql);
    }
}

#[test]
fn a_required_new_column_is_a_review() {
    let rendered = PG.render(
        &MigrationStep::AddColumn {
            table: "orders".into(),
            column: column("must", "integer", false),
        },
        &ctx(),
    );
    assert_eq!(rendered[0].risk, Risk::Review);
}

#[test]
fn views_map_the_scope_schema_and_warn() {
    let view = ViewDef {
        name: "v_totals".into(),
        definition: " SELECT o.id\n   FROM compare_a.orders o;".into(),
        materialized: false,
        depends_on: vec!["orders".into()],
        comment: None,
    };
    let rendered = PG.render(
        &MigrationStep::CreateView {
            view: view.clone(),
            replace: false,
        },
        &ctx(),
    );
    assert_eq!(
        rendered[0].sql,
        "CREATE VIEW compare_b.v_totals AS\nSELECT o.id\n   FROM compare_b.orders o"
    );
    assert_eq!(rendered[0].risk, Risk::Review);
    assert_classified(&rendered[0].sql);
    let materialized = PG.render(
        &MigrationStep::CreateView {
            view: ViewDef {
                materialized: true,
                ..view
            },
            replace: false,
        },
        &ctx(),
    );
    assert!(materialized[0].sql.starts_with("CREATE MATERIALIZED VIEW compare_b.v_totals AS\n"));
    assert!(materialized[0].warnings.iter().any(|w| w.contains("filled again")));
    assert_classified(&materialized[0].sql);
}

fn alter(from: ColumnDef, to: ColumnDef, changes: ColumnChanges) -> Vec<String> {
    let sql = render(MigrationStep::AlterColumn {
        table: "orders".into(),
        from,
        to,
        changes,
    });
    for statement in &sql {
        assert_classified(statement);
    }
    sql
}

#[test]
fn column_alterations_render_in_a_safe_order() {
    let mut to = column("status", "character varying(50)", false);
    to.default = Some("'open'::character varying".into());
    let sql = alter(
        column("status", "character varying(20)", true),
        to,
        ColumnChanges {
            type_name: true,
            nullability: true,
            default: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        sql,
        [
            "ALTER TABLE compare_b.orders ALTER COLUMN status TYPE character varying(50)",
            "ALTER TABLE compare_b.orders ALTER COLUMN status SET DEFAULT 'open'::character varying",
            "-- UPDATE compare_b.orders SET status = ... WHERE status IS NULL;\nALTER TABLE compare_b.orders ALTER COLUMN status SET NOT NULL",
        ]
    );
    let sql = alter(
        column("code", "text", false),
        column("code", "integer", true),
        ColumnChanges {
            type_name: true,
            nullability: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        sql,
        [
            "ALTER TABLE compare_b.orders ALTER COLUMN code TYPE integer USING code::integer",
            "ALTER TABLE compare_b.orders ALTER COLUMN code DROP NOT NULL",
        ]
    );
    let narrowing = PG.render(
        &MigrationStep::AlterColumn {
            table: "orders".into(),
            from: column("n", "bigint", true),
            to: column("n", "integer", true),
            changes: ColumnChanges {
                type_name: true,
                ..ColumnChanges::default()
            },
        },
        &ctx(),
    );
    assert_eq!(narrowing[0].risk, Risk::DataLoss);
    assert!(narrowing[0].warnings[0].contains("may lose data"));
}

#[test]
fn identity_and_generated_changes() {
    let mut always = column("id", "integer", false);
    always.identity = identity(IdentityKind::Always);
    let mut by_default = column("id", "integer", false);
    by_default.identity = identity(IdentityKind::ByDefault);
    let identity_change = ColumnChanges {
        identity: true,
        ..ColumnChanges::default()
    };
    assert_eq!(
        alter(by_default.clone(), always.clone(), identity_change),
        ["ALTER TABLE compare_b.orders ALTER COLUMN id SET GENERATED ALWAYS"]
    );
    assert_eq!(
        alter(column("id", "integer", false), always.clone(), identity_change),
        ["ALTER TABLE compare_b.orders ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY"]
    );
    assert_eq!(
        alter(always, column("id", "integer", false), identity_change),
        ["ALTER TABLE compare_b.orders ALTER COLUMN id DROP IDENTITY"]
    );
    let mut serial = column("id", "integer", false);
    serial.identity = Some(IdentityDef {
        kind: IdentityKind::Serial,
        start: None,
        increment: None,
    });
    let manual = render(MigrationStep::AlterColumn {
        table: "orders".into(),
        from: serial,
        to: by_default,
        changes: identity_change,
    });
    assert!(manual[0].starts_with("-- MANUAL STEP:"), "{manual:?}");

    let mut generated = column("total", "integer", true);
    generated.generated = Some(GeneratedDef {
        expression: "(qty * 3)".into(),
        stored: true,
    });
    let generated_change = ColumnChanges {
        generated: true,
        ..ColumnChanges::default()
    };
    assert_eq!(
        alter(generated.clone(), column("total", "integer", true), generated_change),
        ["ALTER TABLE compare_b.orders ALTER COLUMN total DROP EXPRESSION"]
    );
    assert_eq!(
        alter(column("total", "integer", true), generated, generated_change),
        [
            "ALTER TABLE compare_b.orders DROP COLUMN total",
            "ALTER TABLE compare_b.orders ADD COLUMN total integer GENERATED ALWAYS AS ((qty * 3)) STORED",
        ]
    );
}

#[test]
fn manual_steps_are_comments_and_the_wrapper_is_a_transaction() {
    let manual = PG.render(
        &MigrationStep::Manual {
            reason: "Reorder the columns".into(),
            ddl_hint: Some("CREATE TABLE ...".into()),
        },
        &ctx(),
    );
    assert_eq!(manual[0].sql, "-- MANUAL STEP: Reorder the columns\n--   CREATE TABLE ...");
    assert_eq!(manual[0].risk, Risk::Review);
    let wrapped = PG.wrap(manual.clone(), true);
    let sql: Vec<&str> = wrapped.iter().map(|s| s.sql.as_str()).collect();
    assert_eq!(sql[0], "BEGIN");
    assert_eq!(sql[2], "COMMIT");
    assert_classified("BEGIN");
    assert_classified("COMMIT");
    assert_eq!(PG.wrap(manual, false).len(), 1);
    assert!(PG.features().transactional_ddl);
}

// --- A whole script -----------------------------------------------------------------------------------------------------

fn snapshot(schema: &str, tables: Vec<TableDef>, views: Vec<ViewDef>) -> SchemaSnapshot {
    SchemaSnapshot {
        engine: "postgres".into(),
        server_version: "17.2".into(),
        scope: SnapshotScope::new("shop", Some(schema)),
        case_insensitive_names: false,
        tables,
        views,
        skipped: Vec::new(),
        read_at: String::new(),
    }
}

#[test]
fn a_script_from_two_snapshots_is_ordered_and_classified() {
    let customers = TableDef {
        columns: vec![column("id", "integer", false)],
        primary_key: Some(KeyDef {
            name: Some("customers_pkey".into()),
            columns: vec!["id".into()],
            system_named: false,
        }),
        ..TableDef::new("customers")
    };
    let mut source_orders = TableDef {
        columns: vec![column("id", "integer", false), column("customer_id", "bigint", true)],
        ..TableDef::new("orders")
    };
    source_orders.foreign_keys.push(ForeignKeyDef {
        name: Some("orders_customer_id_fkey".into()),
        columns: vec!["customer_id".into()],
        ref_schema: None,
        ref_table: "customers".into(),
        ref_columns: vec!["id".into()],
        on_update: None,
        on_delete: Some("CASCADE".into()),
        system_named: false,
    });
    let mut target_orders = source_orders.clone();
    target_orders.columns[1].type_name = "integer".into();
    target_orders.columns.push(column("legacy", "text", true));
    let view = |schema: &str| ViewDef {
        name: "v_orders".into(),
        definition: format!(" SELECT id, customer_id\n   FROM {schema}.orders;"),
        materialized: false,
        depends_on: vec!["orders".into()],
        comment: None,
    };
    let source = snapshot("compare_a", vec![customers.clone(), source_orders], vec![view("compare_a")]);
    let target = snapshot(
        "compare_b",
        vec![
            customers,
            target_orders,
            TableDef {
                columns: vec![column("x", "integer", true)],
                ..TableDef::new("only_b")
            },
        ],
        vec![view("compare_b")],
    );
    let options = CompareOptions::default();
    let diff = crate::compare::diff(&source, &target, &options, &PG);
    let include: Vec<String> = diff.items.iter().map(|item| item.id.clone()).collect();
    let script = crate::compare::sync_script(&source, &target, &options, &include, &PG, "2026-11-22 10:00 UTC");
    let body: Vec<&str> = script
        .sql
        .split("\n\n")
        .map(str::trim)
        .filter(|chunk| !chunk.starts_with("-- Schema sync"))
        .collect();
    let statements: Vec<String> = body
        .iter()
        .map(|chunk| {
            chunk
                .lines()
                .filter(|line| !line.starts_with("-- WARNING"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    assert_eq!(
        statements,
        [
            "BEGIN;",
            "ALTER TABLE compare_b.orders DROP CONSTRAINT orders_customer_id_fkey;",
            "DROP VIEW compare_b.v_orders;",
            "DROP TABLE compare_b.only_b;",
            "ALTER TABLE compare_b.orders ALTER COLUMN customer_id TYPE bigint;",
            "ALTER TABLE compare_b.orders DROP COLUMN legacy;",
            "CREATE VIEW compare_b.v_orders AS\nSELECT id, customer_id\n   FROM compare_b.orders;",
            "ALTER TABLE compare_b.orders ADD CONSTRAINT orders_customer_id_fkey FOREIGN KEY (customer_id) REFERENCES compare_b.customers (id) ON DELETE CASCADE;",
            "COMMIT;",
        ],
        "{}",
        script.sql
    );
    for statement in &statements {
        assert_classified(statement);
    }
    assert!(script.transactional);
    assert_eq!(script.statements, 7);
    assert!(script.sql.starts_with("-- Schema sync script generated by Fosta Studio"));
    assert!(script.sql.contains("-- Source: shop.compare_a (postgres 17.2)"));
    assert!(script.sql.contains("-- Warnings: 2 data loss,"), "{}", script.sql);
    assert!(script.sql.contains("-- WARNING: Drops table only_b and all its rows\nDROP TABLE"));
}
