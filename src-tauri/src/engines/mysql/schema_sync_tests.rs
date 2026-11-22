//! Goldens of the MariaDB/MySQL schema compare dialects: normalization across both servers' catalog spellings, type
//! change risks, rendered steps (each checked against the gateway's classifier of both flavours), the missing
//! transaction wrapper and a whole script from two snapshots.

use super::{
    adapter::{MARIADB, MYSQL},
    schema_sync::{MARIADB_SCHEMA_DIALECT as MARIA, MYSQL_SCHEMA_DIALECT as MY},
};
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
    MY.render(&step, &ctx())
}

fn one(step: MigrationStep) -> String {
    let rendered = render(step);
    assert_eq!(rendered.len(), 1, "{rendered:?}");
    rendered.into_iter().next().unwrap().sql
}

/// Every statement parses as DDL (or a destructive DROP) for both flavours, so the gateway asks for the right
/// confirmation when the script runs.
fn assert_classified(sql: &str) {
    for engine in [&MYSQL, &MARIADB] {
        let classification = engine.classify(&ClassifyInput::Statement {
            language: "sql",
            text: sql,
        });
        assert!(!classification.statements.is_empty(), "{sql}");
        for info in &classification.statements {
            assert!(
                matches!(info.category, Category::Ddl | Category::Destructive),
                "{} classifies {sql} as {:?} ({:?})",
                engine.id(),
                info.category,
                info.reason
            );
        }
    }
}

fn column(name: &str, type_name: &str, nullable: bool) -> ColumnDef {
    ColumnDef::new(name, type_name, nullable)
}

fn auto_increment() -> Option<IdentityDef> {
    Some(IdentityDef {
        kind: IdentityKind::AutoIncrement,
        start: None,
        increment: None,
    })
}

// --- Normalization ------------------------------------------------------------------------------------------------------

#[test]
fn types_lose_display_widths_but_keep_booleans_and_zerofill() {
    for dialect in [&MY, &MARIA] {
        let ty = |raw: &str| dialect.normalize_type(raw, Some("compare_a"));
        assert_eq!(ty("int(11)"), "int");
        assert_eq!(ty("INT"), "int");
        assert_eq!(ty("int(10) unsigned"), "int unsigned");
        assert_eq!(ty("int unsigned"), "int unsigned");
        assert_eq!(ty("bigint(20)"), "bigint");
        assert_eq!(ty("tinyint(1)"), "tinyint(1)");
        assert_eq!(ty("tinyint(4)"), "tinyint");
        assert_eq!(ty("int(5) unsigned zerofill"), "int(5) unsigned zerofill");
        assert_eq!(ty("year(4)"), "year");
        assert_eq!(ty("integer"), "int");
        assert_eq!(ty("boolean"), "tinyint(1)");
        assert_eq!(ty("VARCHAR(50)"), "varchar(50)");
        assert_eq!(ty("decimal(12,2)"), "decimal(12,2)");
        assert_eq!(ty("enum('New','Paid')"), "enum('New','Paid')");
        assert_eq!(ty("datetime(3)"), "datetime(3)");
    }
}

#[test]
fn defaults_compare_across_both_servers_spellings() {
    let d = |raw: &str| MY.normalize_default(raw, Some("compare_a"));
    assert_eq!(d("'new'"), "'new'");
    assert_eq!(d("_utf8mb4'new'"), "'new'");
    assert_eq!(d("'it\\'s'"), d("'it''s'"));
    assert_eq!(d("current_timestamp()"), "current_timestamp");
    assert_eq!(d("CURRENT_TIMESTAMP"), "current_timestamp");
    assert_eq!(d("now()"), "current_timestamp");
    assert_eq!(d("current_timestamp(3)"), "current_timestamp(3)");
    assert_eq!(d("'now()'"), "'now()'");
    assert_eq!(
        d("CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP"),
        d("current_timestamp() ON UPDATE current_timestamp()")
    );
    assert_eq!(d("(uuid())"), d("uuid()"));
    assert_eq!(d("0.00"), "0.00");
    assert_eq!(d("NULL"), "");
}

#[test]
fn expressions_and_view_bodies_compare_without_quoting_case_and_the_scope() {
    let a = Some("compare_a");
    let e = |raw: &str| MY.normalize_expression(raw, a);
    // MySQL keeps the outer parentheses and the charset introducer of a check; MariaDB does not.
    assert_eq!(e("(`qty` > 0)"), e("`qty` > 0"));
    assert_eq!(e("(`status` in (_utf8mb4'new',_utf8mb4'paid'))"), e("`status` in ('new','paid')"));
    assert_eq!(e("`qty` > 0"), "qty>0");
    assert_ne!(e("`qty` > 0"), e("`qty` >= 0"));
    assert_eq!(
        MY.normalize_expression(
            "select `compare_a`.`orders`.`id` AS `id` from `compare_a`.`orders` where (`compare_a`.`orders`.`qty` > 10)",
            a
        ),
        MY.normalize_expression(
            "select `compare_b`.`orders`.`id` AS `id` from `compare_b`.`orders` where (`compare_b`.`orders`.`qty` > 10)",
            Some("compare_b")
        )
    );
    assert_eq!(e("`a`   +  `b`"), "a+b");
    assert_eq!(e("`Note` LIKE 'A b'"), "note like 'A b'");
    assert_eq!(e("(`a` > 0) and (`b` > 0)"), "(a>0)and(b>0)");
}

#[test]
fn type_change_risks_follow_widening_and_narrowing() {
    let risk = |from: &str, to: &str| MY.type_change_risk(&column("c", from, true), &column("c", to, true));
    assert_eq!(risk("int(11)", "int"), Risk::None);
    assert_eq!(risk("int", "bigint"), Risk::None);
    assert_eq!(risk("bigint", "int"), Risk::DataLoss);
    assert_eq!(risk("int", "int unsigned"), Risk::DataLoss);
    assert_eq!(risk("int unsigned", "bigint"), Risk::None);
    assert_eq!(risk("varchar(50)", "varchar(200)"), Risk::None);
    assert_eq!(risk("varchar(200)", "varchar(50)"), Risk::DataLoss);
    assert_eq!(risk("varchar(200)", "text"), Risk::None);
    assert_eq!(risk("text", "varchar(200)"), Risk::DataLoss);
    assert_eq!(risk("longtext", "json"), Risk::None);
    assert_eq!(risk("decimal(10,2)", "decimal(12,2)"), Risk::None);
    assert_eq!(risk("decimal(12,2)", "decimal(12,1)"), Risk::DataLoss);
    assert_eq!(risk("int", "decimal(12,2)"), Risk::None);
    assert_eq!(risk("decimal(12,2)", "int"), Risk::DataLoss);
    assert_eq!(risk("datetime(3)", "datetime"), Risk::DataLoss);
    assert_eq!(risk("int", "datetime"), Risk::Review);
    assert_eq!(risk("enum('a','b')", "enum('a')"), Risk::DataLoss);
}

#[test]
fn restrict_equals_no_action_and_ddl_is_not_transactional() {
    for dialect in [&MY, &MARIA] {
        let features = dialect.features();
        assert!(features.restrict_equals_no_action);
        assert!(!features.transactional_ddl);
        let body = vec![RenderedStatement::new("DROP TABLE t")];
        assert_eq!(dialect.wrap(body.clone(), true), body);
    }
}

// --- Rendering ----------------------------------------------------------------------------------------------------------

fn orders() -> TableDef {
    let mut created = column("created_at", "timestamp", false);
    created.default = Some("CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP".into());
    let mut status = column("status", "varchar(20)", false);
    status.default = Some("'new'".into());
    status.collation = Some("utf8mb4_bin".into());
    status.comment = Some("It's the state".into());
    TableDef {
        columns: vec![
            ColumnDef {
                identity: auto_increment(),
                ..column("id", "int", false)
            },
            column("customer_id", "int", false),
            column("qty", "int", false),
            status,
            created,
            ColumnDef {
                generated: Some(GeneratedDef {
                    expression: "`qty` * 2".into(),
                    stored: false,
                }),
                ..column("double_qty", "int", true)
            },
            ColumnDef {
                default: Some("uuid()".into()),
                ..column("token", "char(36)", true)
            },
        ],
        primary_key: Some(KeyDef {
            name: Some("PRIMARY".into()),
            columns: vec!["id".into()],
            system_named: true,
        }),
        checks: vec![
            CheckDef {
                name: Some("orders_qty_check".into()),
                expression: "(`qty` > 0)".into(),
                system_named: false,
            },
            CheckDef {
                name: Some("orders_chk_1".into()),
                expression: "(`qty` < 1000)".into(),
                system_named: true,
            },
        ],
        options: vec![
            ("engine".into(), "InnoDB".into()),
            ("collation".into(), "utf8mb4_0900_ai_ci".into()),
        ],
        ..TableDef::new("orders")
    }
}

#[test]
fn create_table_renders_columns_keys_checks_and_options() {
    let sql = one(MigrationStep::CreateTable { table: orders() });
    assert_eq!(
        sql,
        "CREATE TABLE `compare_b`.`orders` (\n    \
         `id` int NOT NULL AUTO_INCREMENT,\n    \
         `customer_id` int NOT NULL,\n    \
         `qty` int NOT NULL,\n    \
         `status` varchar(20) COLLATE utf8mb4_bin NOT NULL DEFAULT 'new' COMMENT 'It''s the state',\n    \
         `created_at` timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,\n    \
         `double_qty` int GENERATED ALWAYS AS (`qty` * 2) VIRTUAL,\n    \
         `token` char(36) DEFAULT (uuid()),\n    \
         PRIMARY KEY (`id`),\n    \
         CONSTRAINT `orders_qty_check` CHECK ((`qty` > 0)),\n    \
         CHECK ((`qty` < 1000))\n\
         ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci"
    );
    assert_classified(&sql);
}

#[test]
fn table_ddl_shows_indexes_foreign_keys_and_the_comment() {
    let mut table = orders();
    table.comment = Some("Orders".into());
    table.foreign_keys.push(ForeignKeyDef {
        name: Some("orders_customer_fk".into()),
        columns: vec!["customer_id".into()],
        ref_schema: None,
        ref_table: "customers".into(),
        ref_columns: vec!["id".into()],
        on_update: Some("RESTRICT".into()),
        on_delete: Some("CASCADE".into()),
        system_named: false,
    });
    table.indexes = vec![
        IndexDef {
            name: "orders_created_idx".into(),
            parts: vec![IndexPart {
                descending: true,
                ..IndexPart::column("created_at")
            }],
            ..IndexDef::default()
        },
        IndexDef {
            name: "orders_status_token".into(),
            parts: vec![
                IndexPart {
                    expression: Some("`status`(5)".into()),
                    ..IndexPart::default()
                },
                IndexPart {
                    expression: Some("lower(`token`)".into()),
                    ..IndexPart::default()
                },
            ],
            unique: true,
            ..IndexDef::default()
        },
    ];
    let ddl = MY.table_ddl(Some("compare_b"), &table);
    assert!(
        ddl.contains(
            "CONSTRAINT `orders_customer_fk` FOREIGN KEY (`customer_id`) REFERENCES `compare_b`.`customers` (`id`) ON UPDATE RESTRICT ON DELETE CASCADE\n) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci COMMENT='Orders';"
        ),
        "{ddl}"
    );
    assert!(
        ddl.ends_with(
            "\n\nCREATE INDEX `orders_created_idx` ON `compare_b`.`orders` (`created_at` DESC);\n\n\
             CREATE UNIQUE INDEX `orders_status_token` ON `compare_b`.`orders` (`status`(5), (lower(`token`)));"
        ),
        "{ddl}"
    );
    for statement in ddl.split(";\n\n") {
        assert_classified(statement);
    }
}

#[test]
fn identifiers_with_backticks_spaces_and_reserved_words_are_quoted() {
    let table = TableDef {
        columns: vec![column("select", "int", false), column("my `odd` col", "text", true)],
        ..TableDef::new("Order Lines")
    };
    let sql = one(MigrationStep::CreateTable { table });
    assert_eq!(
        sql,
        "CREATE TABLE `compare_b`.`Order Lines` (\n    `select` int NOT NULL,\n    `my ``odd`` col` text\n)"
    );
    assert_classified(&sql);
}

#[test]
fn drops_render_with_data_loss_warnings() {
    let goldens = [
        (
            MigrationStep::DropForeignKey {
                table: "orders".into(),
                name: "orders_customer_fk".into(),
            },
            "ALTER TABLE `compare_b`.`orders` DROP FOREIGN KEY `orders_customer_fk`",
        ),
        (
            MigrationStep::DropView {
                name: "v_orders".into(),
                materialized: false,
            },
            "DROP VIEW `compare_b`.`v_orders`",
        ),
        (
            MigrationStep::DropIndex {
                table: "orders".into(),
                name: "orders_note_idx".into(),
            },
            "DROP INDEX `orders_note_idx` ON `compare_b`.`orders`",
        ),
        (
            MigrationStep::DropConstraint {
                table: "orders".into(),
                kind: ConstraintKind::PrimaryKey,
                name: Some("PRIMARY".into()),
            },
            "ALTER TABLE `compare_b`.`orders` DROP PRIMARY KEY",
        ),
        (
            MigrationStep::DropConstraint {
                table: "orders".into(),
                kind: ConstraintKind::Unique,
                name: Some("orders_email_key".into()),
            },
            "DROP INDEX `orders_email_key` ON `compare_b`.`orders`",
        ),
        (
            MigrationStep::DropConstraint {
                table: "orders".into(),
                kind: ConstraintKind::Check,
                name: Some("orders_qty_check".into()),
            },
            "ALTER TABLE `compare_b`.`orders` DROP CHECK `orders_qty_check`",
        ),
    ];
    for (step, expected) in goldens {
        let sql = one(step);
        assert_eq!(sql, expected);
        assert_classified(&sql);
    }
    let maria = MARIA.render(
        &MigrationStep::DropConstraint {
            table: "orders".into(),
            kind: ConstraintKind::Check,
            name: Some("orders_qty_check".into()),
        },
        &ctx(),
    );
    assert_eq!(maria[0].sql, "ALTER TABLE `compare_b`.`orders` DROP CONSTRAINT `orders_qty_check`");
    assert_classified(&maria[0].sql);

    let table = render(MigrationStep::DropTable { name: "b_only".into() });
    assert_eq!(table[0].sql, "DROP TABLE `compare_b`.`b_only`");
    assert_eq!(table[0].risk, Risk::DataLoss);
    let column = render(MigrationStep::DropColumn {
        table: "customers".into(),
        column: "legacy".into(),
    });
    assert_eq!(column[0].sql, "ALTER TABLE `compare_b`.`customers` DROP COLUMN `legacy`");
    assert_eq!(column[0].risk, Risk::DataLoss);
    assert_classified(&table[0].sql);
    assert_classified(&column[0].sql);
}

#[test]
fn adds_render_constraints_indexes_foreign_keys_and_comments() {
    let mut commented = column("qty", "int", false);
    commented.comment = Some("Many".into());
    let goldens = [
        (
            MigrationStep::AddColumn {
                table: "customers".into(),
                column: ColumnDef {
                    default: Some("'x'".into()),
                    ..column("legacy", "varchar(10)", false)
                },
            },
            "ALTER TABLE `compare_b`.`customers` ADD COLUMN `legacy` varchar(10) NOT NULL DEFAULT 'x'",
        ),
        (
            MigrationStep::AddPrimaryKey {
                table: "items".into(),
                key: KeyDef {
                    name: Some("PRIMARY".into()),
                    columns: vec!["order_id".into(), "sku".into()],
                    system_named: true,
                },
            },
            "ALTER TABLE `compare_b`.`items` ADD PRIMARY KEY (`order_id`, `sku`)",
        ),
        (
            MigrationStep::AddConstraint {
                table: "customers".into(),
                constraint: ConstraintDef::Unique {
                    key: KeyDef {
                        name: Some("customers_email_key".into()),
                        columns: vec!["email".into()],
                        system_named: false,
                    },
                },
            },
            "ALTER TABLE `compare_b`.`customers` ADD CONSTRAINT `customers_email_key` UNIQUE (`email`)",
        ),
        (
            MigrationStep::AddConstraint {
                table: "orders".into(),
                constraint: ConstraintDef::Check {
                    check: CheckDef {
                        name: Some("orders_qty_check".into()),
                        expression: "(`qty` > 0)".into(),
                        system_named: false,
                    },
                },
            },
            "ALTER TABLE `compare_b`.`orders` ADD CONSTRAINT `orders_qty_check` CHECK ((`qty` > 0))",
        ),
        (
            MigrationStep::CreateIndex {
                table: "orders".into(),
                index: IndexDef {
                    name: "orders_placed_idx".into(),
                    parts: vec![
                        IndexPart::column("customer_id"),
                        IndexPart {
                            descending: true,
                            ..IndexPart::column("placed_at")
                        },
                    ],
                    ..IndexDef::default()
                },
            },
            "CREATE INDEX `orders_placed_idx` ON `compare_b`.`orders` (`customer_id`, `placed_at` DESC)",
        ),
        (
            MigrationStep::CreateIndex {
                table: "orders".into(),
                index: IndexDef {
                    name: "orders_note_ft".into(),
                    parts: vec![IndexPart::column("note")],
                    method: Some("FULLTEXT".into()),
                    ..IndexDef::default()
                },
            },
            "ALTER TABLE `compare_b`.`orders` ADD FULLTEXT INDEX `orders_note_ft` (`note`)",
        ),
        (
            MigrationStep::AddForeignKey {
                table: "orders".into(),
                key: ForeignKeyDef {
                    name: Some("orders_customer_fk".into()),
                    columns: vec!["customer_id".into()],
                    ref_schema: Some("compare_a".into()),
                    ref_table: "customers".into(),
                    ref_columns: vec!["id".into()],
                    on_update: Some("NO ACTION".into()),
                    on_delete: Some("SET NULL".into()),
                    system_named: false,
                },
            },
            "ALTER TABLE `compare_b`.`orders` ADD CONSTRAINT `orders_customer_fk` FOREIGN KEY (`customer_id`) REFERENCES `compare_b`.`customers` (`id`) ON DELETE SET NULL",
        ),
        (
            MigrationStep::SetComment {
                target: CommentTarget {
                    kind: "table".into(),
                    name: "orders".into(),
                    column: Some("qty".into()),
                    definition: Some(commented),
                },
                comment: Some("Many".into()),
            },
            "ALTER TABLE `compare_b`.`orders` MODIFY COLUMN `qty` int NOT NULL COMMENT 'Many'",
        ),
    ];
    for (step, expected) in goldens {
        let sql = one(step);
        assert_eq!(sql, expected);
        assert_classified(&sql);
    }
}

#[test]
fn table_comments_and_options_are_manual_steps_the_classifier_can_skip() {
    // The parser behind the gateway cannot read ALTER TABLE … COMMENT / ENGINE / DEFAULT CHARACTER SET; as statements
    // they would turn the whole script into unparsed text.
    let manual = |step: MigrationStep| {
        let rendered = render(step);
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].risk, Risk::Review);
        assert!(rendered[0].sql.lines().all(|line| line.starts_with("--")), "{}", rendered[0].sql);
        rendered[0].sql.clone()
    };
    let table = |column: Option<&str>| CommentTarget {
        kind: "table".into(),
        name: "orders".into(),
        column: column.map(str::to_owned),
        definition: None,
    };
    assert_eq!(
        manual(MigrationStep::SetComment {
            target: table(None),
            comment: Some("It's \\ many".into()),
        }),
        "-- MANUAL STEP: Set the comment of table orders\n--   ALTER TABLE `compare_b`.`orders` COMMENT = 'It''s \\\\ many';"
    );
    assert!(manual(MigrationStep::SetComment {
        target: table(None),
        comment: None,
    })
    .ends_with("COMMENT = '';"));
    assert!(manual(MigrationStep::SetComment {
        target: table(Some("qty")),
        comment: None,
    })
    .contains("MODIFY COLUMN"));
    assert!(manual(MigrationStep::SetTableOption {
        table: "orders".into(),
        name: "engine".into(),
        value: "MyISAM".into(),
    })
    .ends_with("--   ALTER TABLE `compare_b`.`orders` ENGINE = MyISAM;"));
    assert!(manual(MigrationStep::SetTableOption {
        table: "orders".into(),
        name: "collation".into(),
        value: "utf8mb4_bin".into(),
    })
    .ends_with("--   ALTER TABLE `compare_b`.`orders` DEFAULT CHARACTER SET utf8mb4 COLLATE utf8mb4_bin;"));
}

#[test]
fn a_required_new_column_is_a_review() {
    let rendered = render(MigrationStep::AddColumn {
        table: "orders".into(),
        column: column("code", "int", false),
    });
    assert_eq!(rendered[0].risk, Risk::Review);
    assert!(rendered[0].warnings[0].contains("NOT NULL without a default"), "{rendered:?}");
}

#[test]
fn views_map_the_scope_database_and_warn() {
    let view = ViewDef {
        name: "v_big".into(),
        definition: "select `compare_a`.`orders`.`id` AS `id` from `compare_a`.`orders` where (`compare_a`.`orders`.`qty` > 10)".into(),
        ..ViewDef::default()
    };
    let rendered = render(MigrationStep::CreateView {
        view: view.clone(),
        replace: true,
    });
    assert_eq!(
        rendered[0].sql,
        "CREATE OR REPLACE VIEW `compare_b`.`v_big` AS\nselect `compare_b`.`orders`.`id` AS `id` from `compare_b`.`orders` where (`compare_b`.`orders`.`qty` > 10)"
    );
    assert_eq!(rendered[0].risk, Risk::Review);
    assert!(rendered[0].warnings[0].contains("compare_a to compare_b"), "{rendered:?}");
    assert_classified(&rendered[0].sql);
    assert_eq!(
        MY.view_ddl(Some("compare_a"), &view),
        format!("CREATE VIEW `compare_a`.`v_big` AS\n{};", view.definition)
    );
}

fn alter(from: ColumnDef, to: ColumnDef, changes: ColumnChanges) -> Vec<RenderedStatement> {
    let rendered = render(MigrationStep::AlterColumn {
        table: "orders".into(),
        from,
        to,
        changes,
    });
    for statement in &rendered {
        assert_classified(&statement.sql);
    }
    rendered
}

#[test]
fn column_changes_restate_the_target_column_with_only_the_compared_changes() {
    // Type narrowing: the comment, collation and default of the target stay (they are not part of the change).
    let mut from = column("note", "varchar(200)", true);
    from.comment = Some("Free text".into());
    from.collation = Some("utf8mb4_bin".into());
    from.default = Some("'-'".into());
    let mut to = column("note", "varchar(50)", true);
    to.default = Some("'?'".into());
    let rendered = alter(
        from.clone(),
        to.clone(),
        ColumnChanges {
            type_name: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        rendered[0].sql,
        "ALTER TABLE `compare_b`.`orders` MODIFY COLUMN `note` varchar(50) COLLATE utf8mb4_bin DEFAULT '-' COMMENT 'Free text'"
    );
    assert_eq!(rendered[0].risk, Risk::DataLoss);
    assert!(rendered[0].warnings[0].contains("may lose data"), "{rendered:?}");

    // A new default only.
    let rendered = alter(
        from,
        to,
        ColumnChanges {
            nullability: false,
            default: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        rendered[0].sql,
        "ALTER TABLE `compare_b`.`orders` MODIFY COLUMN `note` varchar(200) COLLATE utf8mb4_bin DEFAULT '?' COMMENT 'Free text'"
    );
    let rendered = alter(
        column("placed_at", "datetime", true),
        column("placed_at", "datetime", false),
        ColumnChanges {
            nullability: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        rendered[0].sql,
        "-- UPDATE `compare_b`.`orders` SET `placed_at` = ... WHERE `placed_at` IS NULL;\n\
         ALTER TABLE `compare_b`.`orders` MODIFY COLUMN `placed_at` datetime NOT NULL"
    );
    assert_eq!(rendered[0].risk, Risk::Review);

    // AUTO_INCREMENT and a wider key: one MODIFY.
    let rendered = alter(
        ColumnDef {
            identity: auto_increment(),
            ..column("id", "int", false)
        },
        ColumnDef {
            identity: auto_increment(),
            ..column("id", "bigint", false)
        },
        ColumnChanges {
            type_name: true,
            ..ColumnChanges::default()
        },
    );
    assert_eq!(
        rendered[0].sql,
        "ALTER TABLE `compare_b`.`orders` MODIFY COLUMN `id` bigint NOT NULL AUTO_INCREMENT"
    );
    assert_eq!(rendered[0].risk, Risk::None);
}

#[test]
fn generated_changes_drop_and_add_the_column() {
    let generated = ColumnDef {
        generated: Some(GeneratedDef {
            expression: "`qty` * 3".into(),
            stored: true,
        }),
        ..column("triple", "int", true)
    };
    let rendered = alter(
        column("triple", "int", true),
        generated,
        ColumnChanges {
            generated: true,
            ..ColumnChanges::default()
        },
    );
    let sql: Vec<&str> = rendered.iter().map(|s| s.sql.as_str()).collect();
    assert_eq!(
        sql,
        [
            "ALTER TABLE `compare_b`.`orders` DROP COLUMN `triple`",
            "ALTER TABLE `compare_b`.`orders` ADD COLUMN `triple` int GENERATED ALWAYS AS (`qty` * 3) STORED",
        ]
    );
    assert_eq!(rendered[0].risk, Risk::DataLoss);
}

#[test]
fn manual_steps_are_comments() {
    let rendered = render(MigrationStep::Manual {
        reason: "Reorder the columns of orders".into(),
        ddl_hint: Some("ALTER TABLE ... MODIFY ... AFTER ...".into()),
    });
    assert_eq!(
        rendered[0].sql,
        "-- MANUAL STEP: Reorder the columns of orders\n--   ALTER TABLE ... MODIFY ... AFTER ..."
    );
    assert_eq!(rendered[0].risk, Risk::Review);
}

// --- A whole script -----------------------------------------------------------------------------------------------------

fn snapshot(engine: &str, database: &str, tables: Vec<TableDef>, views: Vec<ViewDef>) -> SchemaSnapshot {
    SchemaSnapshot {
        engine: engine.into(),
        server_version: if engine == "mysql" { "8.4.3" } else { "11.8.2" }.into(),
        scope: SnapshotScope::new(database, Some(database)),
        case_insensitive_names: false,
        tables,
        views,
        skipped: Vec::new(),
        read_at: String::new(),
    }
}

#[test]
fn a_script_from_two_snapshots_is_ordered_classified_and_not_wrapped() {
    let customers = TableDef {
        columns: vec![column("id", "int", false)],
        primary_key: Some(KeyDef {
            name: Some("PRIMARY".into()),
            columns: vec!["id".into()],
            system_named: true,
        }),
        ..TableDef::new("customers")
    };
    let mut source_orders = TableDef {
        columns: vec![
            column("id", "int", false),
            column("customer_id", "bigint", true),
            ColumnDef {
                comment: Some("How many".into()),
                ..column("qty", "int", false)
            },
        ],
        ..TableDef::new("orders")
    };
    source_orders.foreign_keys.push(ForeignKeyDef {
        name: Some("orders_customer_fk".into()),
        columns: vec!["customer_id".into()],
        ref_schema: None,
        ref_table: "customers".into(),
        ref_columns: vec!["id".into()],
        on_update: None,
        on_delete: Some("CASCADE".into()),
        system_named: false,
    });
    // The target is MariaDB-formatted: display widths, RESTRICT for "no action".
    let mut target_orders = source_orders.clone();
    target_orders.columns[0].type_name = "int(11)".into();
    target_orders.columns[1].type_name = "int(11)".into();
    target_orders.columns[2].comment = None;
    target_orders.foreign_keys[0].on_update = Some("RESTRICT".into());
    target_orders.columns.push(column("legacy", "text", true));
    let view = |database: &str| ViewDef {
        name: "v_orders".into(),
        definition: format!(
            "select `{database}`.`orders`.`id` AS `id`,`{database}`.`orders`.`customer_id` AS `customer_id` from `{database}`.`orders`"
        ),
        depends_on: vec!["orders".into()],
        ..ViewDef::default()
    };
    let source = snapshot(
        "mysql",
        "compare_a",
        vec![customers.clone(), source_orders],
        vec![view("compare_a")],
    );
    let mut target_customers = customers;
    target_customers.columns[0].type_name = "int(11)".into();
    let target = snapshot(
        "mariadb",
        "compare_b",
        vec![
            target_customers,
            target_orders,
            TableDef {
                columns: vec![column("x", "int(11)", true)],
                ..TableDef::new("only_b")
            },
        ],
        vec![view("compare_b")],
    );
    let options = CompareOptions {
        transactional: Some(true),
        ..CompareOptions::default()
    };
    let diff = crate::compare::diff(&source, &target, &options, &MARIA);
    let statuses: Vec<(&str, DiffStatus)> = diff.items.iter().map(|item| (item.id.as_str(), item.status)).collect();
    assert_eq!(
        statuses,
        [
            ("table:customers", DiffStatus::Identical),
            ("table:only_b", DiffStatus::OnlyInTarget),
            ("table:orders", DiffStatus::Different),
            ("view:v_orders", DiffStatus::Identical),
        ]
    );
    let include: Vec<String> = diff.items.iter().map(|item| item.id.clone()).collect();
    let script = crate::compare::sync_script(&source, &target, &options, &include, &MARIA, "2026-11-22 10:00 UTC");
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
            "ALTER TABLE `compare_b`.`orders` DROP FOREIGN KEY `orders_customer_fk`;",
            "DROP VIEW `compare_b`.`v_orders`;",
            "DROP TABLE `compare_b`.`only_b`;",
            "ALTER TABLE `compare_b`.`orders` MODIFY COLUMN `customer_id` bigint;",
            "ALTER TABLE `compare_b`.`orders` DROP COLUMN `legacy`;",
            "CREATE VIEW `compare_b`.`v_orders` AS\nselect `compare_b`.`orders`.`id` AS `id`,`compare_b`.`orders`.`customer_id` AS `customer_id` from `compare_b`.`orders`;",
            "ALTER TABLE `compare_b`.`orders` ADD CONSTRAINT `orders_customer_fk` FOREIGN KEY (`customer_id`) REFERENCES `compare_b`.`customers` (`id`) ON UPDATE RESTRICT ON DELETE CASCADE;",
            "ALTER TABLE `compare_b`.`orders` MODIFY COLUMN `qty` int NOT NULL COMMENT 'How many';",
        ],
        "{}",
        script.sql
    );
    for statement in &statements {
        assert_classified(statement);
    }
    assert!(!script.transactional, "MySQL DDL is never wrapped");
    assert!(!script.sql.contains("BEGIN") && !script.sql.contains("COMMIT"), "{}", script.sql);
    assert_eq!(script.statements, 8);
    assert_eq!(script.dialect, "mariadb");
    assert!(script.sql.contains("-- Source: compare_a (mysql 8.4.3)"), "{}", script.sql);
    assert!(script.sql.contains("-- Target: compare_b (mariadb 11.8.2)"), "{}", script.sql);
    assert!(
        script
            .sql
            .contains("-- WARNING: Not atomic: mariadb commits every DDL statement implicitly"),
        "{}",
        script.sql
    );
    // The whole script parses, so the gateway sees every statement and the tables its DROPs touch.
    assert_classified(&script.sql);
    let whole = MARIADB.classify(&ClassifyInput::Statement {
        language: "sql",
        text: &script.sql,
    });
    assert_eq!(whole.statements.len(), 8, "{whole:?}");
}

#[test]
fn created_tables_and_added_columns_carry_their_comments() {
    let table = TableDef {
        columns: vec![ColumnDef {
            comment: Some("Key".into()),
            ..column("id", "int", false)
        }],
        comment: Some("New table".into()),
        ..TableDef::new("a_only")
    };
    let existing = TableDef {
        columns: vec![column("id", "int", false)],
        ..TableDef::new("orders")
    };
    let source_orders = TableDef {
        columns: vec![
            column("id", "int", false),
            ColumnDef {
                comment: Some("Added".into()),
                ..column("note", "text", true)
            },
        ],
        ..existing.clone()
    };
    let source = snapshot("mysql", "compare_a", vec![table.clone(), source_orders], Vec::new());
    let target = snapshot("mysql", "compare_b", vec![existing.clone()], Vec::new());
    let options = CompareOptions::default();
    let include = vec!["table:a_only".to_owned(), "table:orders".to_owned()];
    let script = crate::compare::sync_script(&source, &target, &options, &include, &MY, "now");
    assert!(
        script
            .sql
            .contains("CREATE TABLE `compare_b`.`a_only` (\n    `id` int NOT NULL COMMENT 'Key'\n) COMMENT='New table';"),
        "{}",
        script.sql
    );
    assert!(
        script
            .sql
            .contains("ALTER TABLE `compare_b`.`orders` ADD COLUMN `note` text COMMENT 'Added';"),
        "{}",
        script.sql
    );
    assert!(!script.sql.contains("MANUAL") && !script.sql.contains("MODIFY"), "{}", script.sql);
    assert_eq!(script.statements, 2);

    // Ignored comments stay out of the new table too.
    let ignoring = CompareOptions {
        ignore_comments: true,
        ..CompareOptions::default()
    };
    let script = crate::compare::sync_script(&source, &target, &ignoring, &include, &MY, "now");
    assert!(!script.sql.contains("COMMENT"), "{}", script.sql);
}

#[test]
fn views_keep_sql_security_and_check_option_but_not_the_definer() {
    let options = super::schema_snapshot::view_options(Some("INVOKER".into()), Some("LOCAL".into()), Some("app@%".into()));
    assert_eq!(
        options,
        [
            ("sqlSecurity".to_owned(), "INVOKER".to_owned()),
            ("checkOption".to_owned(), "LOCAL".to_owned()),
            ("definer".to_owned(), "app@%".to_owned()),
        ]
    );
    assert_eq!(
        super::schema_snapshot::view_options(Some("DEFINER".into()), Some("NONE".into()), None),
        [("sqlSecurity".to_owned(), "DEFINER".to_owned())]
    );
    let view = ViewDef {
        name: "v_mine".into(),
        definition: "select `compare_a`.`orders`.`id` AS `id` from `compare_a`.`orders`".into(),
        options,
        ..ViewDef::default()
    };
    let rendered = render(MigrationStep::CreateView {
        view: view.clone(),
        replace: false,
    });
    assert_eq!(
        rendered[0].sql,
        "CREATE SQL SECURITY INVOKER VIEW `compare_b`.`v_mine` AS\n\
         select `compare_b`.`orders`.`id` AS `id` from `compare_b`.`orders`\nWITH LOCAL CHECK OPTION"
    );
    assert!(!rendered[0].sql.contains("DEFINER"));
    assert!(
        rendered[0]
            .warnings
            .iter()
            .any(|w| w.contains("without its DEFINER clause (was app@%)")),
        "{rendered:?}"
    );
    assert_classified(&rendered[0].sql);
    // SQL SECURITY DEFINER: the account running the script becomes the one whose privileges the view reads with.
    let definer = ViewDef {
        options: vec![
            ("sqlSecurity".into(), "DEFINER".into()),
            ("definer".into(), "root@localhost".into()),
        ],
        ..view.clone()
    };
    let rendered = render(MigrationStep::CreateView {
        view: definer,
        replace: false,
    });
    assert!(rendered[0].sql.starts_with("CREATE SQL SECURITY DEFINER VIEW"));
    assert!(rendered[0]
        .warnings
        .iter()
        .any(|w| w.contains("privileges of the account running the script")));
    assert_eq!(
        MY.view_ddl(Some("compare_a"), &view),
        format!(
            "CREATE SQL SECURITY INVOKER VIEW `compare_a`.`v_mine` AS\n{}\nWITH LOCAL CHECK OPTION;",
            view.definition
        )
    );
}
