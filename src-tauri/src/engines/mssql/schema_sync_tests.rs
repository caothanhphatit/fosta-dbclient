//! Goldens of the SQL Server schema compare dialect: normalization of catalog text, type change risks, rendered steps
//! (each checked against the gateway's classifier), the transaction wrapper with its `GO` batches, and a whole script
//! from two snapshots.

use super::{adapter::MSSQL, schema_sync::MSSQL_SCHEMA_DIALECT as MS};
use crate::contract::{
    Category, CheckDef, ClassifyInput, ColumnChanges, ColumnDef, CommentTarget, CompareOptions, ConstraintDef, ConstraintKind, DiffStatus,
    Engine, ForeignKeyDef, GeneratedDef, IdentityDef, IdentityKind, IndexDef, IndexPart, KeyDef, MigrationStep, RenderContext,
    RenderedStatement, Risk, SchemaDialect, SchemaSnapshot, SnapshotScope, TableDef, ViewDef,
};

fn ctx() -> RenderContext {
    RenderContext {
        source_schema: Some("compare_a".into()),
        target_schema: Some("compare_b".into()),
    }
}

fn render(step: MigrationStep) -> Vec<RenderedStatement> {
    MS.render(&step, &ctx())
}

fn one(step: MigrationStep) -> String {
    let rendered = render(step);
    assert_eq!(rendered.len(), 1, "{rendered:?}");
    rendered.into_iter().next().unwrap().sql
}

/// The gateway's view of a script: every statement is classified (none Unknown) and none is lower than `least` allows.
fn categories(sql: &str) -> Vec<Category> {
    let classification = MSSQL.classify(&ClassifyInput::Statement {
        language: "sql",
        text: sql,
    });
    assert!(!classification.statements.is_empty(), "{sql}");
    classification
        .statements
        .iter()
        .map(|info| {
            assert_ne!(info.category, Category::Unknown, "{sql} is unparsed: {:?}", info.reason);
            info.category
        })
        .collect()
}

/// A rendered statement is DDL (or a destructive drop) for the gateway, alone or in its own batch.
fn assert_ddl(sql: &str) {
    for category in categories(sql) {
        assert!(matches!(category, Category::Ddl | Category::Destructive), "{sql}: {category:?}");
    }
}

fn column(name: &str, type_name: &str, nullable: bool) -> ColumnDef {
    ColumnDef::new(name, type_name, nullable)
}

fn identity() -> Option<IdentityDef> {
    Some(IdentityDef {
        kind: IdentityKind::ByDefault,
        start: Some(1),
        increment: Some(1),
    })
}

// --- Normalization ------------------------------------------------------------------------------------------------------

#[test]
fn types_compare_as_the_catalog_spells_them() {
    let ty = |raw: &str| MS.normalize_type(raw, Some("compare_a"));
    assert_eq!(ty("NVARCHAR(MAX)"), "nvarchar(max)");
    assert_eq!(ty("nvarchar(-1)"), "nvarchar(max)");
    assert_eq!(ty("varchar"), "varchar(1)");
    assert_eq!(ty("INTEGER"), "int");
    assert_eq!(ty("numeric(10, 2)"), "decimal(10,2)");
    assert_eq!(ty("decimal"), "decimal(18,0)");
    assert_eq!(ty("decimal(9)"), "decimal(9,0)");
    assert_eq!(ty("datetime2(7)"), "datetime2");
    assert_eq!(ty("datetime2(3)"), "datetime2(3)");
    assert_eq!(ty("float(53)"), "float");
    assert_eq!(ty("float(24)"), "real");
    assert_eq!(ty("timestamp"), "rowversion");
    assert_eq!(ty("double precision"), "float");
}

#[test]
fn defaults_and_expressions_lose_parentheses_brackets_case_and_the_scope() {
    let default = |raw: &str| MS.normalize_default(raw, Some("compare_a"));
    assert_eq!(default("((0))"), "0");
    assert_eq!(default("(0)"), "0");
    assert_eq!(default("(getdate())"), "getdate()");
    assert_eq!(default("(GETDATE())"), "getdate()");
    assert_eq!(default("(N'new')"), "'new'");
    assert_eq!(default("('new')"), "'new'");
    assert_eq!(default("('It''s')"), "'It''s'");
    assert_eq!(default("(NULL)"), "");
    assert_eq!(default("([compare_a].[next_code]())"), "next_code()");
    let expr = |raw: &str, schema: &str| MS.normalize_expression(raw, Some(schema));
    assert_eq!(expr("([qty]>(0))", "compare_a"), "qty>(0)");
    assert_eq!(expr("([qty] > (0))", "compare_a"), "qty>(0)");
    assert_eq!(expr("([note] like N'%x%')", "compare_a"), "note like'%x%'");
    assert_eq!(expr("([placed_at] IS NOT NULL)", "compare_a"), "placed_at is not null");
    assert_eq!(expr("([odd name]>(0))", "compare_a"), "[odd name]>(0)");
    // View bodies: formatting, case, comments, brackets and the scope schema do not count; literals keep their case.
    let a = "SELECT c.id, COUNT(o.id) AS orders -- per customer\nFROM compare_a.customers c\n  LEFT JOIN [compare_a].[orders] o ON o.customer_id = c.id\n WHERE c.status = 'Active'\nGROUP BY c.id";
    let b = "select c.id,count(o.id) as orders /* per customer */ from [compare_b].customers c left join compare_b.orders o on o.customer_id=c.id where c.status='Active' group by c.id";
    assert_eq!(expr(a, "compare_a"), expr(b, "compare_b"));
    assert_ne!(expr(a, "compare_a"), expr(&b.replace("'Active'", "'active'"), "compare_b"));
}

#[test]
fn type_change_risks_follow_widening_and_narrowing() {
    let risk = |from: &str, to: &str| MS.type_change_risk(&column("c", from, true), &column("c", to, true));
    assert_eq!(risk("int", "bigint"), Risk::None);
    assert_eq!(risk("bigint", "int"), Risk::DataLoss);
    assert_eq!(risk("bit", "int"), Risk::None);
    assert_eq!(risk("nvarchar(50)", "nvarchar(200)"), Risk::None);
    assert_eq!(risk("nvarchar(200)", "nvarchar(50)"), Risk::DataLoss);
    assert_eq!(risk("nvarchar(50)", "nvarchar(max)"), Risk::None);
    assert_eq!(risk("nvarchar(max)", "nvarchar(4000)"), Risk::DataLoss);
    assert_eq!(risk("varchar(50)", "nvarchar(50)"), Risk::None);
    assert_eq!(risk("nvarchar(50)", "varchar(50)"), Risk::DataLoss);
    assert_eq!(risk("decimal(10,2)", "decimal(12,2)"), Risk::None);
    assert_eq!(risk("decimal(12,2)", "decimal(12,1)"), Risk::DataLoss);
    assert_eq!(risk("int", "decimal(12,2)"), Risk::None);
    assert_eq!(risk("int", "decimal(5,2)"), Risk::DataLoss);
    assert_eq!(risk("decimal(12,2)", "int"), Risk::DataLoss);
    assert_eq!(risk("datetime2(3)", "datetime2"), Risk::None);
    assert_eq!(risk("datetime2", "datetime2(3)"), Risk::DataLoss);
    assert_eq!(risk("datetime", "datetime2"), Risk::None);
    assert_eq!(risk("datetime2", "datetime"), Risk::DataLoss);
    assert_eq!(risk("date", "datetime2"), Risk::None);
    assert_eq!(risk("datetime", "date"), Risk::DataLoss);
    assert_eq!(risk("int", "nvarchar(20)"), Risk::Review);
    assert_eq!(risk("varbinary(16)", "varbinary(8)"), Risk::DataLoss);
    assert_eq!(risk("INT", "int"), Risk::None);
}

#[test]
fn ddl_is_transactional_and_go_separates_batches() {
    let features = MS.features();
    assert!(features.transactional_ddl && features.columns_held_by_dependents && !features.inline_comments);
    assert_eq!(features.batch_separator, Some("GO"));
    assert_eq!(MS.quote_ident("a]b c"), "[a]]b c]");
}

// --- Rendering ----------------------------------------------------------------------------------------------------------

fn orders() -> TableDef {
    TableDef {
        columns: vec![
            ColumnDef {
                identity: identity(),
                ..column("id", "bigint", false)
            },
            column("customer_id", "int", false),
            ColumnDef {
                default: Some("(N'new')".into()),
                default_name: Some("DF__orders__status__1A2B3C4D".into()),
                ..column("status", "nvarchar(20)", false)
            },
            ColumnDef {
                default: Some("((0))".into()),
                default_name: Some("df_orders_total".into()),
                comment: Some("Including tax".into()),
                ..column("total", "decimal(12,2)", false)
            },
            ColumnDef {
                generated: Some(GeneratedDef {
                    expression: "([total]*(2))".into(),
                    stored: true,
                }),
                ..column("double_total", "decimal(13,2)", true)
            },
            ColumnDef {
                collation: Some("Latin1_General_CS_AS".into()),
                ..column("code", "varchar(10)", true)
            },
        ],
        primary_key: Some(KeyDef {
            name: Some("PK__orders__3213E83F".into()),
            columns: vec!["id".into()],
            system_named: true,
        }),
        uniques: vec![KeyDef {
            name: Some("uq_orders_code".into()),
            columns: vec!["code".into()],
            system_named: false,
        }],
        checks: vec![CheckDef {
            name: Some("CK__orders__total__1".into()),
            expression: "([total]>=(0))".into(),
            system_named: true,
        }],
        indexes: vec![IndexDef {
            name: "ix_orders_status".into(),
            parts: vec![
                IndexPart::column("status"),
                IndexPart {
                    descending: true,
                    ..IndexPart::column("id")
                },
            ],
            include: vec!["total".into()],
            predicate: Some("([status]<>'done')".into()),
            ..IndexDef::default()
        }],
        foreign_keys: vec![ForeignKeyDef {
            name: Some("fk_orders_customer".into()),
            columns: vec!["customer_id".into()],
            ref_schema: None,
            ref_table: "customers".into(),
            ref_columns: vec!["id".into()],
            on_update: Some("NO ACTION".into()),
            on_delete: Some("CASCADE".into()),
            system_named: false,
        }],
        comment: Some("The orders".into()),
        ..TableDef::new("orders")
    }
}

#[test]
fn create_table_renders_columns_defaults_keys_and_checks() {
    let mut table = orders();
    table.foreign_keys.clear();
    table.indexes.clear();
    let sql = one(MigrationStep::CreateTable { table });
    assert_eq!(
        sql,
        "CREATE TABLE [compare_b].[orders] (\n    \
         [id] bigint IDENTITY(1,1) NOT NULL,\n    \
         [customer_id] int NOT NULL,\n    \
         [status] nvarchar(20) NOT NULL CONSTRAINT [DF_orders_status] DEFAULT (N'new'),\n    \
         [total] decimal(12,2) NOT NULL CONSTRAINT [df_orders_total] DEFAULT ((0)),\n    \
         [double_total] AS ([total]*(2)) PERSISTED,\n    \
         [code] varchar(10) COLLATE Latin1_General_CS_AS NULL,\n    \
         PRIMARY KEY ([id]),\n    \
         CONSTRAINT [uq_orders_code] UNIQUE ([code]),\n    \
         CHECK ([total]>=(0))\n)"
    );
    // Computed columns make it unreadable for the parser; alone in its batch it is recognized.
    assert_ddl(&sql);
}

#[test]
fn table_ddl_shows_indexes_foreign_keys_and_comments() {
    let ddl = MS.table_ddl(Some("compare_b"), &orders());
    assert!(
        ddl.contains("    CONSTRAINT [fk_orders_customer] FOREIGN KEY ([customer_id]) REFERENCES [compare_b].[customers] ([id]) ON DELETE CASCADE\n);"),
        "{ddl}"
    );
    assert!(
        ddl.contains(
            "CREATE INDEX [ix_orders_status] ON [compare_b].[orders] ([status], [id] DESC) INCLUDE ([total]) WHERE ([status]<>'done');"
        ),
        "{ddl}"
    );
    assert!(
        ddl.ends_with(
            "EXEC sys.sp_addextendedproperty @name = N'MS_Description', @value = N'Including tax', @level0type = N'SCHEMA', \
             @level0name = N'compare_b', @level1type = N'TABLE', @level1name = N'orders', @level2type = N'COLUMN', \
             @level2name = N'total';"
        ),
        "{ddl}"
    );
    let view = MS.view_ddl(
        Some("compare_b"),
        &ViewDef {
            name: "v".into(),
            definition: "SELECT 1 AS n;".into(),
            comment: Some("A view".into()),
            ..ViewDef::default()
        },
    );
    assert!(
        view.starts_with("CREATE VIEW [compare_b].[v] AS\nSELECT 1 AS n;\n\nEXEC sys.sp_addextendedproperty"),
        "{view}"
    );
}

#[test]
fn drops_render_by_name_with_data_loss_warnings() {
    let cases = [
        (
            MigrationStep::DropForeignKey {
                table: "orders".into(),
                name: "fk_orders_customer".into(),
            },
            "ALTER TABLE [compare_b].[orders] DROP CONSTRAINT [fk_orders_customer]",
        ),
        (
            MigrationStep::DropView {
                name: "v".into(),
                materialized: false,
            },
            "DROP VIEW [compare_b].[v]",
        ),
        (
            MigrationStep::DropIndex {
                table: "orders".into(),
                name: "ix_orders_status".into(),
            },
            "DROP INDEX [ix_orders_status] ON [compare_b].[orders]",
        ),
        (
            MigrationStep::DropConstraint {
                table: "orders".into(),
                kind: ConstraintKind::PrimaryKey,
                name: Some("PK__orders__3213E83F".into()),
            },
            "ALTER TABLE [compare_b].[orders] DROP CONSTRAINT [PK__orders__3213E83F]",
        ),
        (MigrationStep::DropTable { name: "old".into() }, "DROP TABLE [compare_b].[old]"),
        (
            MigrationStep::DropColumn {
                table: "orders".into(),
                column: "legacy".into(),
            },
            "ALTER TABLE [compare_b].[orders] DROP COLUMN [legacy]",
        ),
    ];
    for (step, expected) in cases {
        let sql = one(step);
        assert_eq!(sql, expected);
        assert_ddl(&sql);
    }
    assert_eq!(render(MigrationStep::DropTable { name: "old".into() })[0].risk, Risk::DataLoss);
    let unnamed = one(MigrationStep::DropConstraint {
        table: "orders".into(),
        kind: ConstraintKind::Check,
        name: None,
    });
    assert!(
        unnamed.starts_with("-- MANUAL STEP: Drop the unnamed check constraint of orders"),
        "{unnamed}"
    );
}

#[test]
fn adds_render_columns_constraints_indexes_and_foreign_keys() {
    let table = orders();
    let cases = [
        (
            MigrationStep::AddColumn {
                table: "orders".into(),
                column: table.columns[2].clone(),
            },
            "ALTER TABLE [compare_b].[orders] ADD [status] nvarchar(20) NOT NULL CONSTRAINT [DF_orders_status] DEFAULT (N'new')",
        ),
        (
            MigrationStep::AddColumn {
                table: "orders".into(),
                column: table.columns[4].clone(),
            },
            "ALTER TABLE [compare_b].[orders] ADD [double_total] AS ([total]*(2)) PERSISTED",
        ),
        (
            MigrationStep::AddPrimaryKey {
                table: "orders".into(),
                key: table.primary_key.clone().unwrap(),
            },
            "ALTER TABLE [compare_b].[orders] ADD PRIMARY KEY ([id])",
        ),
        (
            MigrationStep::AddConstraint {
                table: "orders".into(),
                constraint: ConstraintDef::Unique {
                    key: table.uniques[0].clone(),
                },
            },
            "ALTER TABLE [compare_b].[orders] ADD CONSTRAINT [uq_orders_code] UNIQUE ([code])",
        ),
        (
            MigrationStep::AddConstraint {
                table: "orders".into(),
                constraint: ConstraintDef::Check {
                    check: CheckDef {
                        name: Some("ck_orders_qty".into()),
                        expression: "([qty]>(0))".into(),
                        system_named: false,
                    },
                },
            },
            "ALTER TABLE [compare_b].[orders] ADD CONSTRAINT [ck_orders_qty] CHECK ([qty]>(0))",
        ),
        (
            MigrationStep::CreateIndex {
                table: "orders".into(),
                index: table.indexes[0].clone(),
            },
            "CREATE INDEX [ix_orders_status] ON [compare_b].[orders] ([status], [id] DESC) INCLUDE ([total]) WHERE ([status]<>'done')",
        ),
        (
            MigrationStep::CreateIndex {
                table: "orders".into(),
                index: IndexDef {
                    name: "cx_orders".into(),
                    parts: vec![IndexPart::column("code")],
                    unique: true,
                    method: Some("CLUSTERED".into()),
                    ..IndexDef::default()
                },
            },
            "CREATE UNIQUE CLUSTERED INDEX [cx_orders] ON [compare_b].[orders] ([code])",
        ),
        (
            MigrationStep::AddForeignKey {
                table: "orders".into(),
                key: ForeignKeyDef {
                    ref_schema: Some("compare_a".into()),
                    on_update: Some("SET_NULL".into()),
                    ..table.foreign_keys[0].clone()
                },
            },
            "ALTER TABLE [compare_b].[orders] ADD CONSTRAINT [fk_orders_customer] FOREIGN KEY ([customer_id]) REFERENCES \
             [compare_b].[customers] ([id]) ON DELETE CASCADE ON UPDATE SET NULL",
        ),
    ];
    for (step, expected) in cases {
        let sql = one(step);
        assert_eq!(sql, expected);
        assert_ddl(&sql);
    }
    let required = render(MigrationStep::AddColumn {
        table: "orders".into(),
        column: column("qty", "int", false),
    });
    assert_eq!(required[0].risk, Risk::Review);
    assert!(required[0].warnings[0].contains("fails if the table has rows"));
}

#[test]
fn comments_set_or_drop_the_description_whether_or_not_it_is_there() {
    let target = |column: Option<&str>| CommentTarget {
        kind: "table".into(),
        name: "orders".into(),
        column: column.map(str::to_owned),
        definition: None,
    };
    let set = one(MigrationStep::SetComment {
        target: target(Some("total")),
        comment: Some("It's net".into()),
    });
    assert_eq!(
        set,
        "IF EXISTS (SELECT 1 FROM sys.fn_listextendedproperty(N'MS_Description', N'SCHEMA', N'compare_b', N'TABLE', N'orders', \
         N'COLUMN', N'total'))\n    EXEC sys.sp_updateextendedproperty @name = N'MS_Description', @value = N'It''s net', \
         @level0type = N'SCHEMA', @level0name = N'compare_b', @level1type = N'TABLE', @level1name = N'orders', \
         @level2type = N'COLUMN', @level2name = N'total'\nELSE\n    EXEC sys.sp_addextendedproperty @name = N'MS_Description', \
         @value = N'It''s net', @level0type = N'SCHEMA', @level0name = N'compare_b', @level1type = N'TABLE', \
         @level1name = N'orders', @level2type = N'COLUMN', @level2name = N'total'"
    );
    let dropped = one(MigrationStep::SetComment {
        target: CommentTarget {
            kind: "view".into(),
            ..target(None)
        },
        comment: None,
    });
    assert!(
        dropped.contains("N'VIEW', N'orders', NULL, NULL))\n    EXEC sys.sp_dropextendedproperty"),
        "{dropped}"
    );
    // Procedure calls: the gateway sees writes, never unparsed text.
    for sql in [&set, &dropped] {
        assert_eq!(categories(sql), [Category::Write]);
    }
}

fn alter(from: ColumnDef, to: ColumnDef, changes: ColumnChanges) -> Vec<RenderedStatement> {
    render(MigrationStep::AlterColumn {
        table: "orders".into(),
        from,
        to,
        changes,
    })
}

fn sqls(rendered: &[RenderedStatement]) -> Vec<&str> {
    rendered.iter().map(|r| r.sql.as_str()).collect()
}

#[test]
fn column_changes_restate_type_and_nullability_and_cycle_defaults() {
    let total = orders().columns[3].clone();
    // A type change of a column with a default: the default goes and comes back around it.
    let wider = ColumnDef {
        type_name: "decimal(14,2)".into(),
        default_name: Some("DF__orders__total__99AA88BB".into()),
        ..total.clone()
    };
    let rendered = alter(
        total.clone(),
        wider,
        ColumnChanges {
            type_name: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        sqls(&rendered),
        [
            "ALTER TABLE [compare_b].[orders] DROP CONSTRAINT [df_orders_total]",
            "ALTER TABLE [compare_b].[orders] ALTER COLUMN [total] decimal(14,2) NOT NULL",
            "ALTER TABLE [compare_b].[orders] ADD CONSTRAINT [df_orders_total] DEFAULT ((0)) FOR [total]",
        ]
    );
    for statement in &rendered {
        assert_ddl(&statement.sql);
    }
    // Only the default: drop the target's by name, add the source's under a name the script chooses.
    let rendered = alter(
        total.clone(),
        ColumnDef {
            default: Some("((1))".into()),
            default_name: Some("DF__orders__total__99AA88BB".into()),
            ..total.clone()
        },
        ColumnChanges {
            default: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        sqls(&rendered),
        [
            "ALTER TABLE [compare_b].[orders] DROP CONSTRAINT [df_orders_total]",
            "ALTER TABLE [compare_b].[orders] ADD CONSTRAINT [DF_orders_total] DEFAULT ((1)) FOR [total]",
        ]
    );
    // NOT NULL gets an UPDATE stub and a warning; narrowing warns of data loss.
    let note = column("note", "nvarchar(200)", true);
    let rendered = alter(
        note.clone(),
        column("note", "nvarchar(50)", false),
        ColumnChanges {
            type_name: true,
            nullability: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(rendered.len(), 1);
    assert_eq!(
        rendered[0].sql,
        "-- UPDATE [compare_b].[orders] SET [note] = ... WHERE [note] IS NULL;\n\
         ALTER TABLE [compare_b].[orders] ALTER COLUMN [note] nvarchar(50) NOT NULL"
    );
    assert_eq!(rendered[0].risk, Risk::DataLoss);
    assert_eq!(rendered[0].warnings.len(), 2);
    assert_ddl(&rendered[0].sql);
    // IDENTITY cannot change in place: a manual step, and the rest still runs.
    let id = orders().columns[0].clone();
    let rendered = alter(
        id.clone(),
        ColumnDef {
            identity: None,
            ..column("id", "bigint", true)
        },
        ColumnChanges {
            identity: true,
            nullability: true,
            ..ColumnChanges::default()
        },
    );
    assert!(
        rendered[0]
            .sql
            .starts_with("-- MANUAL STEP: SQL Server cannot add or remove IDENTITY"),
        "{rendered:?}"
    );
    assert_eq!(rendered[1].sql, "ALTER TABLE [compare_b].[orders] ALTER COLUMN [id] bigint NULL");
}

#[test]
fn computed_columns_are_dropped_and_added_again() {
    let computed = orders().columns[4].clone();
    let rendered = alter(
        computed.clone(),
        ColumnDef {
            generated: Some(GeneratedDef {
                expression: "([total]*(3))".into(),
                stored: false,
            }),
            ..computed
        },
        ColumnChanges {
            generated: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        sqls(&rendered),
        [
            "ALTER TABLE [compare_b].[orders] DROP COLUMN [double_total]",
            "ALTER TABLE [compare_b].[orders] ADD [double_total] AS ([total]*(3))",
        ]
    );
    assert_eq!(rendered[0].risk, Risk::Review);
    for statement in &rendered {
        assert_ddl(&statement.sql);
    }
}

#[test]
fn views_map_the_scope_schema_and_warn() {
    let rendered = render(MigrationStep::CreateView {
        view: ViewDef {
            name: "v_big".into(),
            definition: "SELECT id FROM compare_a.orders o JOIN [compare_a].[items] i ON i.order_id = o.id WHERE o.note <> 'compare_a.x';"
                .into(),
            ..ViewDef::default()
        },
        replace: false,
    });
    assert_eq!(
        rendered[0].sql,
        "CREATE VIEW [compare_b].[v_big] AS\nSELECT id FROM [compare_b].orders o JOIN [compare_b].[items] i ON i.order_id = o.id \
         WHERE o.note <> 'compare_a.x'"
    );
    assert_eq!(rendered[0].risk, Risk::Review);
    assert_ddl(&rendered[0].sql);
}

#[test]
fn manual_steps_are_comments() {
    let sql = one(MigrationStep::SetTableOption {
        table: "orders".into(),
        name: "x".into(),
        value: "y".into(),
    });
    assert!(sql.starts_with("-- MANUAL STEP: Set option x = y on orders by hand"));
}

#[test]
fn the_wrapper_puts_views_and_unparsed_ddl_in_batches_of_their_own() {
    let body = vec![
        RenderedStatement::new("ALTER TABLE [s].[t] DROP CONSTRAINT [fk]"),
        RenderedStatement::new("ALTER TABLE [s].[t] ALTER COLUMN [c] bigint NOT NULL"),
        RenderedStatement::new("ALTER TABLE [s].[t] ADD CONSTRAINT [fk] FOREIGN KEY ([c]) REFERENCES [s].[p] ([id])"),
        RenderedStatement::new("CREATE VIEW [s].[v] AS\nSELECT 1 AS n"),
        RenderedStatement::new("-- MANUAL STEP: something"),
    ];
    let texts = |wrapped: Vec<RenderedStatement>| wrapped.into_iter().map(|s| s.sql).collect::<Vec<_>>();
    let guard = "IF @@ERROR <> 0 OR @@TRANCOUNT = 0 SET NOEXEC ON";
    assert_eq!(
        texts(MS.wrap(body.clone(), true)),
        [
            "SET XACT_ABORT ON",
            "BEGIN TRANSACTION",
            "ALTER TABLE [s].[t] DROP CONSTRAINT [fk]",
            "GO",
            guard,
            "GO",
            "ALTER TABLE [s].[t] ALTER COLUMN [c] bigint NOT NULL",
            "GO",
            guard,
            "GO",
            "ALTER TABLE [s].[t] ADD CONSTRAINT [fk] FOREIGN KEY ([c]) REFERENCES [s].[p] ([id])",
            "GO",
            guard,
            "GO",
            "CREATE VIEW [s].[v] AS\nSELECT 1 AS n",
            "GO",
            guard,
            "GO",
            "-- MANUAL STEP: something",
            "COMMIT TRANSACTION",
            "GO",
            "SET NOEXEC OFF",
        ]
    );
    let plain = texts(MS.wrap(body[..1].to_vec(), false));
    assert_eq!(plain, ["ALTER TABLE [s].[t] DROP CONSTRAINT [fk]"]);
    let unwrapped = texts(MS.wrap(body[1..2].to_vec(), false));
    assert_eq!(unwrapped, ["ALTER TABLE [s].[t] ALTER COLUMN [c] bigint NOT NULL"]);
}

// --- Whole scripts ------------------------------------------------------------------------------------------------------

fn snapshot(schema: &str, tables: Vec<TableDef>, views: Vec<ViewDef>) -> SchemaSnapshot {
    SchemaSnapshot {
        engine: "mssql".into(),
        server_version: "16.0.4135.4".into(),
        scope: SnapshotScope::new("fosta_contract", Some(schema)),
        case_insensitive_names: true,
        tables,
        views,
        skipped: Vec::new(),
        read_at: String::new(),
    }
}

#[test]
fn a_script_from_two_snapshots_runs_in_batches_the_gateway_reads() {
    let customers = |pk: &str| TableDef {
        columns: vec![ColumnDef {
            identity: identity(),
            ..column("id", "int", false)
        }],
        primary_key: Some(KeyDef {
            name: Some(pk.into()),
            columns: vec!["id".into()],
            system_named: true,
        }),
        ..TableDef::new("customers")
    };
    let orders = |schema: &str, id_type: &str, status: &str, default_name: &str| TableDef {
        columns: vec![
            ColumnDef {
                identity: identity(),
                ..column("id", id_type, false)
            },
            column("customer_id", "int", false),
            ColumnDef {
                default: Some(format!("('{status}')")),
                default_name: Some(default_name.into()),
                ..column("status", "nvarchar(20)", false)
            },
        ],
        primary_key: Some(KeyDef {
            name: Some(format!("PK__orders__{}", if schema == "compare_a" { "AAAA" } else { "BBBB" })),
            columns: vec!["id".into()],
            system_named: true,
        }),
        foreign_keys: vec![ForeignKeyDef {
            name: Some("fk_orders_customer".into()),
            columns: vec!["customer_id".into()],
            ref_schema: None,
            ref_table: "customers".into(),
            ref_columns: vec!["id".into()],
            on_delete: Some("CASCADE".into()),
            on_update: Some("NO ACTION".into()),
            system_named: false,
        }],
        ..TableDef::new("orders")
    };
    let view = |schema: &str| ViewDef {
        name: "v_orders".into(),
        definition: format!("SELECT o.id, o.status FROM [{schema}].[orders] o"),
        depends_on: vec!["orders".into()],
        ..ViewDef::default()
    };
    let source = snapshot(
        "compare_a",
        vec![
            customers("PK__customer__1"),
            orders("compare_a", "bigint", "new", "DF__orders__statu__0011AABB"),
        ],
        vec![view("compare_a")],
    );
    let target = snapshot(
        "compare_b",
        vec![
            customers("PK__customer__2"),
            orders("compare_b", "int", "open", "DF__orders__statu__CCDD0011"),
        ],
        vec![view("compare_b")],
    );
    let options = CompareOptions::default();
    let diff = crate::compare::diff(&source, &target, &options, &MS);
    let statuses: Vec<(&str, DiffStatus)> = diff.items.iter().map(|item| (item.id.as_str(), item.status)).collect();
    // System-named keys and defaults match by definition.
    assert_eq!(
        statuses,
        [
            ("table:customers", DiffStatus::Identical),
            ("table:orders", DiffStatus::Different),
            ("view:v_orders", DiffStatus::Identical),
        ]
    );
    let include: Vec<String> = diff.items.iter().map(|item| item.id.clone()).collect();
    let script = crate::compare::sync_script(&source, &target, &options, &include, &MS, "2026-11-22 10:00 UTC");
    let body: Vec<String> = script
        .sql
        .split("\n\n")
        .map(str::trim)
        .filter(|chunk| !chunk.starts_with("-- Schema sync"))
        .map(|chunk| {
            chunk
                .lines()
                .filter(|line| !line.starts_with("-- WARNING"))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    let guard = "IF @@ERROR <> 0 OR @@TRANCOUNT = 0 SET NOEXEC ON;";
    assert_eq!(
        body,
        [
            "SET XACT_ABORT ON;",
            "BEGIN TRANSACTION;",
            "DROP VIEW [compare_b].[v_orders];",
            "ALTER TABLE [compare_b].[orders] DROP CONSTRAINT [PK__orders__BBBB];",
            "GO",
            guard,
            "GO",
            "ALTER TABLE [compare_b].[orders] ALTER COLUMN [id] bigint NOT NULL;",
            "GO",
            guard,
            "GO",
            "ALTER TABLE [compare_b].[orders] DROP CONSTRAINT [DF__orders__statu__CCDD0011];",
            "GO",
            guard,
            "GO",
            "ALTER TABLE [compare_b].[orders] ADD CONSTRAINT [DF_orders_status] DEFAULT ('new') FOR [status];",
            "GO",
            guard,
            "GO",
            "ALTER TABLE [compare_b].[orders] ADD PRIMARY KEY ([id]);",
            "GO",
            guard,
            "GO",
            "CREATE VIEW [compare_b].[v_orders] AS\nSELECT o.id, o.status FROM [compare_b].[orders] o;",
            "GO",
            guard,
            "GO",
            "COMMIT TRANSACTION;",
            "GO",
            "SET NOEXEC OFF;",
        ],
        "{}",
        script.sql
    );
    assert!(script.transactional);
    assert_eq!(script.statements, 7);
    assert_eq!(script.dialect, "mssql");
    assert!(
        script.sql.contains("-- Target: fosta_contract.compare_b (mssql 16.0.4135.4)"),
        "{}",
        script.sql
    );
    // The gateway reads every batch: the DROPs keep their tables, nothing is unparsed.
    let whole = MSSQL.classify(&ClassifyInput::Statement {
        language: "sql",
        text: &script.sql,
    });
    assert!(whole.statements.iter().all(|s| s.category != Category::Unknown), "{whole:?}");
    let drop = whole.statements.iter().find(|s| s.kind == "DROP VIEW").expect("the view drop");
    assert_eq!(drop.tables, ["[compare_b].[v_orders]"]);
    assert_eq!(whole.worst(), Some(Category::Destructive));
}

#[test]
fn views_keep_their_column_list_and_with_attributes() {
    let (body, options) = super::schema_snapshot::view_parts(
        "/* AS */ CREATE VIEW [s].[v] (a, [b c]) WITH SCHEMABINDING, VIEW_METADATA AS SELECT x AS a, y AS [b c] FROM s.t WITH CHECK OPTION",
    );
    assert_eq!(body, "SELECT x AS a, y AS [b c] FROM s.t WITH CHECK OPTION");
    assert_eq!(
        options,
        [
            ("columns".to_owned(), "a, [b c]".to_owned()),
            ("schemabinding".to_owned(), "on".to_owned()),
            ("viewMetadata".to_owned(), "on".to_owned()),
        ]
    );
    let (_, plain) = super::schema_snapshot::view_parts("CREATE VIEW v AS SELECT (1) AS x");
    assert!(plain.is_empty(), "{plain:?}");
    let rendered = render(MigrationStep::CreateView {
        view: ViewDef {
            name: "v".into(),
            definition: body,
            options,
            ..ViewDef::default()
        },
        replace: false,
    });
    assert_eq!(
        rendered[0].sql,
        "CREATE VIEW [compare_b].[v] (a, [b c]) WITH SCHEMABINDING, VIEW_METADATA AS\nSELECT x AS a, y AS [b c] FROM s.t WITH CHECK OPTION"
    );
    assert_ddl(&rendered[0].sql);
}

#[test]
fn a_persisted_computed_column_makes_way_for_the_identity_column_change() {
    // SQL Server error 11424: the identity column of a table with a persisted computed column cannot change type.
    let orders = |id_type: &str, pk: &str| TableDef {
        columns: vec![
            ColumnDef {
                identity: identity(),
                ..column("id", id_type, false)
            },
            ColumnDef {
                default: Some("((0))".into()),
                default_name: Some("df_orders_total".into()),
                ..column("total", "decimal(12,2)", false)
            },
            ColumnDef {
                generated: Some(GeneratedDef {
                    expression: "([total]*(2))".into(),
                    stored: true,
                }),
                ..column("double_total", "decimal(13,2)", false)
            },
        ],
        primary_key: Some(KeyDef {
            name: Some(pk.into()),
            columns: vec!["id".into()],
            system_named: false,
        }),
        indexes: vec![IndexDef {
            name: "ix_orders_double".into(),
            parts: vec![IndexPart::column("double_total")],
            ..IndexDef::default()
        }],
        ..TableDef::new("orders")
    };
    let source = snapshot("compare_a", vec![orders("bigint", "pk_orders")], Vec::new());
    let target = snapshot("compare_b", vec![orders("int", "pk_orders")], Vec::new());
    let options = CompareOptions::default();
    let include = vec!["table:orders".to_owned()];
    let script = crate::compare::sync_script(&source, &target, &options, &include, &MS, "2026-11-22 10:00 UTC");
    let statements: Vec<&str> = script
        .sql
        .lines()
        .filter(|line| line.starts_with("ALTER") || line.starts_with("CREATE") || line.starts_with("DROP"))
        .collect();
    assert_eq!(
        statements,
        [
            "ALTER TABLE [compare_b].[orders] DROP CONSTRAINT [pk_orders];",
            "DROP INDEX [ix_orders_double] ON [compare_b].[orders];",
            "ALTER TABLE [compare_b].[orders] DROP COLUMN [double_total];",
            "ALTER TABLE [compare_b].[orders] ALTER COLUMN [id] bigint NOT NULL;",
            "ALTER TABLE [compare_b].[orders] ADD [double_total] AS ([total]*(2)) PERSISTED NOT NULL;",
            "ALTER TABLE [compare_b].[orders] ADD CONSTRAINT [pk_orders] PRIMARY KEY ([id]);",
            "CREATE INDEX [ix_orders_double] ON [compare_b].[orders] ([double_total]);",
        ],
        "{}",
        script.sql
    );
    let whole = MSSQL.classify(&ClassifyInput::Statement {
        language: "sql",
        text: &script.sql,
    });
    assert!(whole.statements.iter().all(|s| s.category != Category::Unknown), "{whole:?}");
}

#[test]
fn a_collation_change_cycles_the_default_too() {
    let code = ColumnDef {
        default: Some("('x')".into()),
        default_name: Some("df_orders_code".into()),
        ..orders().columns[5].clone()
    };
    let folded = ColumnDef {
        collation: Some("Latin1_General_CI_AS".into()),
        ..code.clone()
    };
    let rendered = alter(
        code,
        folded,
        ColumnChanges {
            collation: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        sqls(&rendered),
        [
            "ALTER TABLE [compare_b].[orders] DROP CONSTRAINT [df_orders_code]",
            "ALTER TABLE [compare_b].[orders] ALTER COLUMN [code] varchar(10) COLLATE Latin1_General_CI_AS NULL",
            "ALTER TABLE [compare_b].[orders] ADD CONSTRAINT [df_orders_code] DEFAULT ('x') FOR [code]",
        ]
    );
}
