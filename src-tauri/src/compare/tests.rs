//! Diff tests against a test-only dialect whose normalizers only trim, collapse whitespace and drop the scope schema.

use super::*;
use crate::contract::{
    strip_schema_qualifier, ChangeKind, CheckDef, ColumnDef, CompareOptions, DialectFeatures, DiffItem, DiffStatus, ForeignKeyDef,
    IdentityDef, IdentityKind, IndexDef, IndexPart, KeyDef, MigrationStep, RenderContext, RenderedStatement, Risk, SchemaDialect,
    SchemaDiff, SchemaSnapshot, SkippedObject, SnapshotScope, TableDef, ViewDef,
};

struct FakeDialect {
    restrict_equals_no_action: bool,
    /// Columns held by their dependents (SQL Server).
    held: bool,
    /// Foreign keys inside the table, and tables rebuilt for anything but an added column (SQLite).
    inline: bool,
}

const FAKE: FakeDialect = FakeDialect {
    restrict_equals_no_action: false,
    held: false,
    inline: false,
};

fn normalize(raw: &str, schema: Option<&str>) -> String {
    let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    schema.map_or(text.clone(), |schema| strip_schema_qualifier(&text, schema))
}

/// The length in `varchar(n)`.
fn length(type_name: &str) -> Option<u32> {
    type_name.split_once('(')?.1.trim_end_matches(')').parse().ok()
}

impl SchemaDialect for FakeDialect {
    fn normalize_type(&self, raw: &str, schema: Option<&str>) -> String {
        normalize(raw, schema).to_lowercase()
    }
    fn normalize_default(&self, raw: &str, schema: Option<&str>) -> String {
        normalize(raw, schema)
    }
    fn normalize_expression(&self, raw: &str, schema: Option<&str>) -> String {
        normalize(raw, schema)
    }
    fn type_change_risk(&self, from: &ColumnDef, to: &ColumnDef) -> Risk {
        match (length(&from.type_name), length(&to.type_name)) {
            (Some(from), Some(to)) if to < from => Risk::DataLoss,
            (Some(_), Some(_)) => Risk::None,
            _ => Risk::Review,
        }
    }
    fn features(&self) -> DialectFeatures {
        DialectFeatures {
            transactional_ddl: true,
            alter_column_type: true,
            drop_column: true,
            restrict_equals_no_action: self.restrict_equals_no_action,
            batch_separator: None,
            inline_comments: false,
            columns_held_by_dependents: self.held,
            inline_foreign_keys: self.inline,
        }
    }
    fn needs_rebuild(&self, item: &DiffItem, _source: &TableDef, _target: &TableDef) -> bool {
        self.inline && item.changes.iter().any(|c| !(c.part == "column" && c.what == ChangeKind::Added))
    }
    fn quote_ident(&self, name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
    fn table_ddl(&self, schema: Option<&str>, table: &TableDef) -> String {
        let columns: Vec<String> = table
            .columns
            .iter()
            .map(|c| {
                let mut line = format!("  {} {}", self.quote_ident(&c.name), c.type_name);
                if !c.nullable {
                    line.push_str(" NOT NULL");
                }
                if let Some(default) = &c.default {
                    line.push_str(&format!(" DEFAULT {default}"));
                }
                line
            })
            .collect();
        let schema = schema.map(|s| format!("{}.", self.quote_ident(s))).unwrap_or_default();
        format!(
            "CREATE TABLE {schema}{} (\n{}\n);",
            self.quote_ident(&table.name),
            columns.join(",\n")
        )
    }
    fn view_ddl(&self, _schema: Option<&str>, view: &ViewDef) -> String {
        format!("CREATE VIEW {} AS {};", self.quote_ident(&view.name), view.definition)
    }
    fn render(&self, step: &MigrationStep, _ctx: &RenderContext) -> Vec<RenderedStatement> {
        vec![RenderedStatement::new(format!("-- {step:?}"))]
    }
    fn wrap(&self, body: Vec<RenderedStatement>, transactional: bool) -> Vec<RenderedStatement> {
        if !transactional {
            return body;
        }
        let mut out = vec![RenderedStatement::new("BEGIN;")];
        out.extend(body);
        out.push(RenderedStatement::new("COMMIT;"));
        out
    }
}

// --- Builders -----------------------------------------------------------------------------------------------------------

fn snapshot(schema: &str, tables: Vec<TableDef>, views: Vec<ViewDef>) -> SchemaSnapshot {
    SchemaSnapshot {
        engine: "postgres".into(),
        server_version: "17.2".into(),
        scope: SnapshotScope::new("shop", Some(schema)),
        case_insensitive_names: false,
        tables,
        views,
        skipped: Vec::new(),
        read_at: "2026-11-22T10:00:00Z".into(),
    }
}

fn col(name: &str, type_name: &str) -> ColumnDef {
    ColumnDef::new(name, type_name, true)
}

fn not_null(name: &str, type_name: &str) -> ColumnDef {
    ColumnDef::new(name, type_name, false)
}

fn table(name: &str, columns: Vec<ColumnDef>) -> TableDef {
    TableDef {
        columns,
        ..TableDef::new(name)
    }
}

fn orders() -> TableDef {
    let mut id = not_null("id", "integer");
    id.identity = Some(IdentityDef {
        kind: IdentityKind::ByDefault,
        start: Some(1),
        increment: Some(1),
    });
    TableDef {
        primary_key: Some(KeyDef {
            name: Some("orders_pkey".into()),
            columns: vec!["id".into()],
            system_named: false,
        }),
        ..table(
            "orders",
            vec![
                id,
                not_null("customer_id", "integer"),
                col("status", "varchar(20)"),
                col("note", "text"),
            ],
        )
    }
}

fn fk(name: Option<&str>, ref_schema: Option<&str>, on_delete: Option<&str>) -> ForeignKeyDef {
    ForeignKeyDef {
        name: name.map(str::to_owned),
        columns: vec!["customer_id".into()],
        ref_schema: ref_schema.map(str::to_owned),
        ref_table: "customers".into(),
        ref_columns: vec!["id".into()],
        on_update: None,
        on_delete: on_delete.map(str::to_owned),
        system_named: name.is_none(),
    }
}

fn view(name: &str, definition: &str) -> ViewDef {
    ViewDef {
        name: name.into(),
        definition: definition.into(),
        ..ViewDef::default()
    }
}

fn run(source: &SchemaSnapshot, target: &SchemaSnapshot, options: &CompareOptions) -> SchemaDiff {
    diff(source, target, options, &FAKE)
}

fn tables(source: Vec<TableDef>, target: Vec<TableDef>) -> SchemaDiff {
    run(
        &snapshot("a", source, Vec::new()),
        &snapshot("b", target, Vec::new()),
        &CompareOptions::default(),
    )
}

fn item<'d>(diff: &'d SchemaDiff, id: &str) -> &'d DiffItem {
    diff.items
        .iter()
        .find(|i| i.id == id)
        .unwrap_or_else(|| panic!("no item {id} in {:?}", ids(diff)))
}

fn ids(diff: &SchemaDiff) -> Vec<(String, DiffStatus)> {
    diff.items.iter().map(|i| (i.id.clone(), i.status)).collect()
}

/// (part, name, what) of each change.
fn changes(item: &DiffItem) -> Vec<(String, String, ChangeKind)> {
    item.changes.iter().map(|c| (c.part.clone(), c.name.clone(), c.what)).collect()
}

// --- Matching -----------------------------------------------------------------------------------------------------------

#[test]
fn tables_match_by_name_with_stable_ids_and_statuses() {
    let diff = tables(
        vec![
            orders(),
            table("only_a", vec![col("x", "int")]),
            table("same", vec![col("x", "int")]),
        ],
        vec![
            table("same", vec![col("x", "int")]),
            table("only_b", vec![col("y", "int")]),
            orders(),
        ],
    );
    assert_eq!(
        ids(&diff),
        [
            ("table:only_a".to_owned(), DiffStatus::OnlyInSource),
            ("table:only_b".to_owned(), DiffStatus::OnlyInTarget),
            ("table:orders".to_owned(), DiffStatus::Identical),
            ("table:same".to_owned(), DiffStatus::Identical),
        ]
    );
    assert_eq!(item(&diff, "table:only_b").risk, Risk::DataLoss);
    assert_eq!(item(&diff, "table:only_a").risk, Risk::None);
    // Same input, same ids and order.
    let again = tables(
        vec![
            orders(),
            table("only_a", vec![col("x", "int")]),
            table("same", vec![col("x", "int")]),
        ],
        vec![
            table("same", vec![col("x", "int")]),
            table("only_b", vec![col("y", "int")]),
            orders(),
        ],
    );
    assert_eq!(ids(&again), ids(&diff));
    assert!(item(&diff, "table:orders")
        .source_ddl
        .as_deref()
        .unwrap()
        .starts_with("CREATE TABLE \"b\".\"orders\""));
}

#[test]
fn names_fold_case_when_either_snapshot_is_case_insensitive() {
    let source = snapshot("a", vec![table("Orders", vec![col("ID", "int")])], Vec::new());
    let target = snapshot("b", vec![table("orders", vec![col("id", "int")])], Vec::new());
    let sensitive = run(&source, &target, &CompareOptions::default());
    assert_eq!(sensitive.items.len(), 2);

    let mut folding = target.clone();
    folding.case_insensitive_names = true;
    let diff = run(&source, &folding, &CompareOptions::default());
    assert_eq!(ids(&diff), [("table:Orders".to_owned(), DiffStatus::Identical)]);

    let forced = CompareOptions {
        case_insensitive_names: Some(true),
        ..CompareOptions::default()
    };
    assert_eq!(run(&source, &target, &forced).items.len(), 1);
}

#[test]
fn column_changes_carry_fields_and_risks() {
    let mut target = orders();
    target.columns[2] = not_null("status", "varchar(50)");
    target.columns.push(col("legacy", "text"));
    let mut source = orders();
    source.columns.push(not_null("total", "numeric(12,2)"));
    let diff = tables(vec![source], vec![target]);
    let orders = item(&diff, "table:orders");
    assert_eq!(orders.status, DiffStatus::Different);
    assert_eq!(
        changes(orders),
        [
            ("column".to_owned(), "status".to_owned(), ChangeKind::Modified),
            ("column".to_owned(), "total".to_owned(), ChangeKind::Added),
            ("column".to_owned(), "legacy".to_owned(), ChangeKind::Removed),
        ]
    );
    let status = &orders.changes[0];
    // varchar(50) NOT NULL on the target becomes the source's varchar(20) NULL: narrowing.
    assert_eq!(status.risk, Risk::DataLoss);
    let fields: Vec<&str> = status.fields.iter().map(|f| f.field.as_str()).collect();
    assert_eq!(fields, ["type", "nullable"]);
    assert_eq!(status.fields[0].source.as_deref(), Some("varchar(20)"));
    assert_eq!(status.fields[0].target.as_deref(), Some("varchar(50)"));
    assert_eq!(orders.changes[1].risk, Risk::Review, "NOT NULL without a default");
    assert_eq!(orders.changes[2].risk, Risk::DataLoss);
    assert_eq!(orders.risk, Risk::DataLoss);
}

#[test]
fn making_a_column_not_null_is_a_review() {
    let mut source = orders();
    source.columns[3].nullable = false;
    let diff = tables(vec![source], vec![orders()]);
    let change = &item(&diff, "table:orders").changes[0];
    assert_eq!(change.risk, Risk::Review);
    assert!(change.note.as_deref().unwrap().contains("NULLs"));
}

#[test]
fn system_named_constraints_match_by_definition() {
    let key = |name: &str| KeyDef {
        name: Some(name.into()),
        columns: vec!["id".into()],
        system_named: true,
    };
    let check = |name: &str| CheckDef {
        name: Some(name.into()),
        expression: "(status  IN ('NEW', 'PAID'))".into(),
        system_named: true,
    };
    let mut source = orders();
    source.primary_key = Some(key("PK__orders__3213E83F"));
    source.checks = vec![check("CK__orders__status__1A2B")];
    source.foreign_keys = vec![fk(None, None, None)];
    let mut target = orders();
    target.primary_key = Some(key("PK__orders__A1B2C3D4"));
    target.checks = vec![check("CK__orders__status__9F8E")];
    target.foreign_keys = vec![ForeignKeyDef {
        name: Some("FK__orders__cust__77AA".into()),
        ..fk(None, None, None)
    }];
    let diff = tables(vec![source], vec![target]);
    assert_eq!(
        item(&diff, "table:orders").status,
        DiffStatus::Identical,
        "{:?}",
        item(&diff, "table:orders").changes
    );
}

#[test]
fn user_named_constraints_compare_names_unless_ignored() {
    let unique = |name: &str| KeyDef {
        name: Some(name.into()),
        columns: vec!["status".into()],
        system_named: false,
    };
    let mut source = orders();
    source.uniques = vec![unique("uq_status")];
    let mut target = orders();
    target.uniques = vec![unique("orders_status_key")];
    let diff = tables(vec![source.clone()], vec![target.clone()]);
    let change = &item(&diff, "table:orders").changes[0];
    assert_eq!((change.part.as_str(), change.what), ("unique", ChangeKind::Modified));
    assert_eq!(change.fields[0].field, "name");
    assert_eq!(change.risk, Risk::None, "a rename only");

    let options = CompareOptions {
        ignore_constraint_names: true,
        ..CompareOptions::default()
    };
    let diff = run(
        &snapshot("a", vec![source], Vec::new()),
        &snapshot("b", vec![target], Vec::new()),
        &options,
    );
    assert_eq!(item(&diff, "table:orders").status, DiffStatus::Identical);
}

#[test]
fn constraints_with_the_same_name_and_another_definition_are_modified() {
    let check = |expression: &str| CheckDef {
        name: Some("ck_status".into()),
        expression: expression.into(),
        system_named: false,
    };
    let mut source = orders();
    source.checks = vec![check("status IN ('NEW', 'PAID', 'SHIPPED')")];
    let mut target = orders();
    target.checks = vec![check("status IN ('NEW', 'PAID')")];
    let diff = tables(vec![source], vec![target]);
    let change = &item(&diff, "table:orders").changes[0];
    assert_eq!(
        (change.part.as_str(), change.name.as_str(), change.what),
        ("check", "ck_status", ChangeKind::Modified)
    );
    assert_eq!(change.fields[0].field, "expression");
    assert_eq!(change.risk, Risk::Review);
}

#[test]
fn indexes_compare_parts_predicates_and_names() {
    let index = |name: &str, descending: bool, predicate: Option<&str>| IndexDef {
        name: name.into(),
        parts: vec![
            IndexPart::column("status"),
            IndexPart {
                descending,
                ..IndexPart::column("customer_id")
            },
        ],
        unique: false,
        method: Some("btree".into()),
        predicate: predicate.map(str::to_owned),
        include: Vec::new(),
        system_named: false,
    };
    let mut source = orders();
    source.indexes = vec![index("ix_status", true, Some("status <> 'DONE'")), index("ix_new", false, None)];
    let mut target = orders();
    target.indexes = vec![
        index("ix_status", false, Some("status <> 'DONE'")),
        index("ix_old", false, Some("x")),
    ];
    let diff = tables(vec![source.clone()], vec![target]);
    assert_eq!(
        changes(item(&diff, "table:orders")),
        [
            ("index".to_owned(), "ix_status".to_owned(), ChangeKind::Modified),
            ("index".to_owned(), "ix_new".to_owned(), ChangeKind::Added),
            ("index".to_owned(), "ix_old".to_owned(), ChangeKind::Removed),
        ]
    );
    assert_eq!(
        item(&diff, "table:orders").changes[0].fields[0].source.as_deref(),
        Some("status, customer_id DESC")
    );

    // A renamed but otherwise equal index differs by name, unless index names are ignored.
    let mut renamed = source.clone();
    renamed.indexes[1].name = "ix_customer".into();
    let diff = tables(vec![source.clone()], vec![renamed.clone()]);
    assert_eq!(item(&diff, "table:orders").changes[0].fields[0].field, "name");
    let options = CompareOptions {
        ignore_index_names: true,
        ..CompareOptions::default()
    };
    let diff = run(
        &snapshot("a", vec![source], Vec::new()),
        &snapshot("b", vec![renamed], Vec::new()),
        &options,
    );
    assert_eq!(item(&diff, "table:orders").status, DiffStatus::Identical);
}

#[test]
fn foreign_keys_map_the_scope_schema_and_treat_missing_actions_as_no_action() {
    let mut source = orders();
    source.foreign_keys = vec![fk(Some("fk_customer"), Some("a"), Some("NO ACTION"))];
    let mut target = orders();
    target.foreign_keys = vec![fk(Some("fk_customer"), Some("b"), None)];
    let diff = tables(vec![source.clone()], vec![target.clone()]);
    assert_eq!(item(&diff, "table:orders").status, DiffStatus::Identical);

    // A reference to another schema is a real difference.
    target.foreign_keys = vec![fk(Some("fk_customer"), Some("crm"), None)];
    let diff = tables(vec![source.clone()], vec![target.clone()]);
    let change = &item(&diff, "table:orders").changes[0];
    assert_eq!(change.fields[0].field, "references");
    assert_eq!(change.fields[0].target.as_deref(), Some("crm.customers(id)"));

    // RESTRICT equals NO ACTION only where the dialect says so.
    target.foreign_keys = vec![fk(Some("fk_customer"), None, Some("restrict"))];
    let diff = tables(vec![source.clone()], vec![target.clone()]);
    assert_eq!(item(&diff, "table:orders").changes[0].fields[0].field, "onDelete");
    let mysql = FakeDialect {
        restrict_equals_no_action: true,
        held: false,
        inline: false,
    };
    let diff = super::diff(
        &snapshot("a", vec![source], Vec::new()),
        &snapshot("b", vec![target], Vec::new()),
        &CompareOptions::default(),
        &mysql,
    );
    assert_eq!(item(&diff, "table:orders").status, DiffStatus::Identical);
}

#[test]
fn added_foreign_keys_are_a_review() {
    let mut source = orders();
    source.foreign_keys = vec![fk(Some("fk_customer"), None, Some("CASCADE"))];
    let diff = tables(vec![source], vec![orders()]);
    let change = &item(&diff, "table:orders").changes[0];
    assert_eq!(
        (change.part.as_str(), change.what, change.risk),
        ("foreignKey", ChangeKind::Added, Risk::Review)
    );
}

#[test]
fn primary_keys_are_one_item() {
    let mut source = orders();
    source.primary_key.as_mut().unwrap().columns = vec!["id".into(), "customer_id".into()];
    let diff = tables(vec![source], vec![orders()]);
    let change = &item(&diff, "table:orders").changes[0];
    assert_eq!(
        (change.part.as_str(), change.what, change.risk),
        ("primaryKey", ChangeKind::Modified, Risk::Review)
    );

    let mut keyless = orders();
    keyless.primary_key = None;
    let diff = tables(vec![orders()], vec![keyless]);
    assert_eq!(item(&diff, "table:orders").changes[0].what, ChangeKind::Added);
}

// --- Ignore options -----------------------------------------------------------------------------------------------------

#[test]
fn column_order_is_ignored_by_default() {
    let mut reordered = orders();
    reordered.columns.swap(2, 3);
    let diff = tables(vec![orders()], vec![reordered.clone()]);
    let orders_item = item(&diff, "table:orders");
    assert_eq!(orders_item.status, DiffStatus::Identical);
    // The target's DDL follows the source's column order, so the side-by-side text is equal too.
    assert_eq!(orders_item.source_ddl, orders_item.target_ddl);

    let options = CompareOptions {
        ignore_column_order: false,
        ..CompareOptions::default()
    };
    let diff = run(
        &snapshot("a", vec![orders()], Vec::new()),
        &snapshot("b", vec![reordered], Vec::new()),
        &options,
    );
    let change = &item(&diff, "table:orders").changes[0];
    assert_eq!((change.part.as_str(), change.risk), ("columnOrder", Risk::Review));
    assert_eq!(change.fields[0].source.as_deref(), Some("id, customer_id, status, note"));
}

#[test]
fn comments_defaults_and_collations_follow_their_options() {
    let mut source = orders();
    source.comment = Some("Customer orders".into());
    source.columns[2].default = Some("'NEW'".into());
    source.columns[2].collation = Some("C".into());
    source.columns[3].comment = Some("Free text".into());
    let target = orders();
    let compare = |options: CompareOptions| {
        run(
            &snapshot("a", vec![source.clone()], Vec::new()),
            &snapshot("b", vec![target.clone()], Vec::new()),
            &options,
        )
    };
    let diff = compare(CompareOptions::default());
    let parts: Vec<(String, String)> = item(&diff, "table:orders")
        .changes
        .iter()
        .flat_map(|c| c.fields.iter().map(move |f| (c.part.clone(), f.field.clone())))
        .collect();
    assert_eq!(
        parts,
        [
            ("column".to_owned(), "default".to_owned()),
            ("column".to_owned(), "comment".to_owned()),
            ("comment".to_owned(), "comment".to_owned()),
        ]
    );
    let diff = compare(CompareOptions {
        ignore_comments: true,
        ignore_defaults: true,
        ..CompareOptions::default()
    });
    let orders_item = item(&diff, "table:orders");
    assert_eq!(orders_item.status, DiffStatus::Identical);
    assert!(!orders_item.source_ddl.as_deref().unwrap().contains("DEFAULT"));
    let diff = compare(CompareOptions {
        ignore_collation: false,
        ..CompareOptions::default()
    });
    assert!(item(&diff, "table:orders").changes[0].fields.iter().any(|f| f.field == "collation"));
}

#[test]
fn serial_defaults_are_not_compared() {
    let serial = |default: &str| {
        let mut column = not_null("id", "integer");
        column.default = Some(default.into());
        column.identity = Some(IdentityDef {
            kind: IdentityKind::Serial,
            start: None,
            increment: None,
        });
        table("t", vec![column])
    };
    let diff = tables(
        vec![serial("nextval('a.t_id_seq'::regclass)")],
        vec![serial("nextval('t_id_seq'::regclass)")],
    );
    assert_eq!(item(&diff, "table:t").status, DiffStatus::Identical);
}

#[test]
fn table_options_are_shown_for_review() {
    let mut source = orders();
    source.options = vec![("engine".into(), "InnoDB".into()), ("collation".into(), "utf8mb4_bin".into())];
    let mut target = orders();
    target.options = vec![
        ("ENGINE".into(), "MyISAM".into()),
        ("collation".into(), "utf8mb4_general_ci".into()),
    ];
    let diff = tables(vec![source], vec![target]);
    let orders_item = item(&diff, "table:orders");
    assert_eq!(
        changes(orders_item),
        [("option".to_owned(), "engine".to_owned(), ChangeKind::Modified)]
    );
    assert_eq!(orders_item.risk, Risk::Review);
}

#[test]
fn excluded_names_and_kinds_are_left_out() {
    let source = snapshot(
        "a",
        vec![
            orders(),
            table("flyway_schema_history", vec![col("x", "int")]),
            table("knex_migrations_lock", vec![]),
        ],
        vec![view("v", "SELECT 1")],
    );
    let target = snapshot("b", vec![orders(), table("__efmigrationshistory", vec![])], Vec::new());
    let diff = run(&source, &target, &CompareOptions::default());
    assert_eq!(
        ids(&diff),
        [
            ("table:orders".to_owned(), DiffStatus::Identical),
            ("view:v".to_owned(), DiffStatus::OnlyInSource),
        ]
    );
    let skipped: Vec<&str> = diff.skipped.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(skipped, ["flyway_schema_history", "knex_migrations_lock", "__efmigrationshistory"]);

    let options = CompareOptions {
        include_kinds: vec!["table".into()],
        exclude: vec!["ord?rs".into()],
        ..CompareOptions::default()
    };
    let diff = run(&source, &target, &options);
    assert_eq!(
        ids(&diff),
        [
            ("table:__efmigrationshistory".to_owned(), DiffStatus::OnlyInTarget),
            ("table:flyway_schema_history".to_owned(), DiffStatus::OnlyInSource),
            ("table:knex_migrations_lock".to_owned(), DiffStatus::OnlyInSource),
        ]
    );
}

#[test]
fn skipped_objects_of_both_sides_are_merged_without_duplicates() {
    let trigger = SkippedObject {
        kind: "trigger".into(),
        name: "audit".into(),
        reason: "Triggers are not compared in this version".into(),
    };
    let mut source = snapshot("a", Vec::new(), Vec::new());
    source.skipped = vec![trigger.clone()];
    let mut target = snapshot("b", Vec::new(), Vec::new());
    target.skipped = vec![trigger.clone()];
    assert_eq!(run(&source, &target, &CompareOptions::default()).skipped, [trigger]);
}

// --- Views --------------------------------------------------------------------------------------------------------------

#[test]
fn view_bodies_compare_without_the_scope_schema() {
    let source = snapshot(
        "compare_a",
        Vec::new(),
        vec![
            view("v_totals", "SELECT customer_id, sum(total)\n  FROM compare_a.orders GROUP BY 1"),
            view("v_changed", "SELECT 1"),
        ],
    );
    let target = snapshot(
        "compare_b",
        Vec::new(),
        vec![
            view("v_totals", "SELECT customer_id, sum(total) FROM \"compare_b\".orders GROUP BY 1"),
            view("v_changed", "SELECT 2"),
        ],
    );
    let diff = run(&source, &target, &CompareOptions::default());
    assert_eq!(
        ids(&diff),
        [
            ("view:v_changed".to_owned(), DiffStatus::Different),
            ("view:v_totals".to_owned(), DiffStatus::Identical),
        ]
    );
    let changed = item(&diff, "view:v_changed");
    assert_eq!(
        changes(changed),
        [("definition".to_owned(), "v_changed".to_owned(), ChangeKind::Modified)]
    );
    assert_eq!(changed.risk, Risk::None);
    assert_eq!(changed.source_ddl.as_deref(), Some("CREATE VIEW \"v_changed\" AS SELECT 1;"));
}

#[test]
fn changed_materialized_views_need_a_refresh() {
    let mut source = view("mv", "SELECT 1");
    source.materialized = true;
    let mut target = source.clone();
    target.definition = "SELECT 2".into();
    let diff = run(
        &snapshot("a", Vec::new(), vec![source]),
        &snapshot("b", Vec::new(), vec![target]),
        &CompareOptions::default(),
    );
    let mv = item(&diff, "view:mv");
    assert_eq!(mv.risk, Risk::Review);
    assert!(mv.changes[0].note.as_deref().unwrap().contains("refreshed"));
}

// --- Renames ------------------------------------------------------------------------------------------------------------

#[test]
fn a_lone_drop_and_add_of_the_same_type_is_a_possible_rename() {
    let mut source = orders();
    source.columns[3].name = "remark".into();
    let diff = tables(vec![source.clone()], vec![orders()]);
    let orders_item = item(&diff, "table:orders");
    let added = orders_item.changes.iter().find(|c| c.what == ChangeKind::Added).unwrap();
    let removed = orders_item.changes.iter().find(|c| c.what == ChangeKind::Removed).unwrap();
    assert_eq!(added.possible_rename.as_deref(), Some("note"));
    assert_eq!(removed.possible_rename.as_deref(), Some("remark"));
    assert_eq!((added.risk, removed.risk), (Risk::Review, Risk::DataLoss));
    assert!(added.note.as_deref().unwrap().contains("Possible rename of note to remark"));

    let options = CompareOptions {
        detect_renames: false,
        ..CompareOptions::default()
    };
    let diff = run(
        &snapshot("a", vec![source.clone()], Vec::new()),
        &snapshot("b", vec![orders()], Vec::new()),
        &options,
    );
    assert!(item(&diff, "table:orders").changes.iter().all(|c| c.possible_rename.is_none()));

    // Another type, or two candidates: no hint.
    let mut retyped = source.clone();
    retyped.columns[3].type_name = "integer".into();
    let diff = tables(vec![retyped], vec![orders()]);
    assert!(item(&diff, "table:orders").changes.iter().all(|c| c.possible_rename.is_none()));
    let mut two = source;
    two.columns.push(col("other", "text"));
    let diff = tables(vec![two], vec![orders()]);
    assert!(item(&diff, "table:orders").changes.iter().all(|c| c.possible_rename.is_none()));
}

// --- Engines and options ------------------------------------------------------------------------------------------------

#[test]
fn only_the_same_engine_or_mysql_and_mariadb_compare() {
    assert!(engines_compatible("postgres", "postgres"));
    assert!(engines_compatible("mysql", "mariadb") && engines_compatible("mariadb", "mysql"));
    assert!(!engines_compatible("postgres", "mssql"));
    let source = snapshot("a", Vec::new(), Vec::new());
    let mut target = source.clone();
    target.engine = "sqlite".into();
    assert!(check_compatible(&source, &source).is_ok());
    assert!(check_compatible(&source, &target).unwrap_err().to_string().contains("same engine"));
}

#[test]
fn globs_match_case_insensitively() {
    assert!(glob_match("knex_migrations*", "knex_migrations_lock"));
    assert!(glob_match("knex_migrations*", "KNEX_MIGRATIONS"));
    assert!(glob_match("__EFMigrationsHistory", "__efmigrationshistory"));
    assert!(glob_match("a?c*d", "abcxxd"));
    assert!(glob_match("*", ""));
    assert!(!glob_match("orders", "orders_archive"));
    assert!(!glob_match("a*b", "acd"));
}

#[test]
fn the_diff_round_trips_through_json() {
    let mut source = orders();
    source.columns.push(col("extra", "text"));
    let diff = tables(vec![source], vec![orders()]);
    let back: SchemaDiff = serde_json::from_value(serde_json::to_value(&diff).unwrap()).unwrap();
    assert_eq!(back, diff);
}

#[test]
fn the_fake_dialect_wraps_in_a_transaction() {
    let body = FAKE.render(&MigrationStep::DropTable { name: "x".into() }, &RenderContext::default());
    let wrapped = FAKE.wrap(body, true);
    assert_eq!(wrapped.first().unwrap().sql, "BEGIN;");
    assert_eq!(wrapped.len(), 3);
}

// --- Planner ------------------------------------------------------------------------------------------------------------

/// The steps of a plan over every item of the diff (or `include`) as short labels, and the plan's warnings.
fn planned(source: &SchemaSnapshot, target: &SchemaSnapshot, include: Option<&[&str]>) -> (Vec<String>, Vec<String>) {
    planned_with(&FAKE, source, target, include)
}

fn planned_with(
    dialect: &FakeDialect,
    source: &SchemaSnapshot,
    target: &SchemaSnapshot,
    include: Option<&[&str]>,
) -> (Vec<String>, Vec<String>) {
    let options = CompareOptions::default();
    let diff = super::diff(source, target, &options, dialect);
    let include: Vec<String> = match include {
        Some(ids) => ids.iter().map(|id| (*id).to_owned()).collect(),
        None => diff.items.iter().map(|item| item.id.clone()).collect(),
    };
    let plan = plan(&diff, &include, source, target, &options, dialect);
    let steps = plan
        .steps
        .iter()
        .map(|planned| match &planned.step {
            MigrationStep::DropForeignKey { table, name } => format!("drop fk {table}.{name}"),
            MigrationStep::DropView { name, .. } => format!("drop view {name}"),
            MigrationStep::DropIndex { name, .. } => format!("drop index {name}"),
            MigrationStep::DropConstraint { table, name, .. } => {
                format!("drop constraint {table}.{}", name.clone().unwrap_or_default())
            }
            MigrationStep::DropTable { name } => format!("drop table {name}"),
            MigrationStep::CreateTable { table } => format!("create table {}", table.name),
            MigrationStep::AddColumn { table, column } => format!("add column {table}.{}", column.name),
            MigrationStep::AlterColumn { table, to, .. } => format!("alter column {table}.{}", to.name),
            MigrationStep::DropColumn { table, column } => format!("drop column {table}.{column}"),
            MigrationStep::AddPrimaryKey { table, .. } => format!("add pk {table}"),
            MigrationStep::AddConstraint { table, .. } => format!("add constraint {table}"),
            MigrationStep::CreateIndex { index, .. } => format!("create index {}", index.name),
            MigrationStep::CreateView { view, .. } => format!("create view {}", view.name),
            MigrationStep::AddForeignKey { table, key } => format!("add fk {table}.{}", key.name.clone().unwrap_or_default()),
            MigrationStep::SetComment { target, .. } => format!("comment {}", target.name),
            other => format!("{other:?}"),
        })
        .collect();
    (steps, plan.warnings.into_iter().map(|w| w.message).collect())
}

fn customers() -> TableDef {
    TableDef {
        primary_key: Some(KeyDef {
            name: Some("customers_pkey".into()),
            columns: vec!["id".into()],
            system_named: false,
        }),
        ..table("customers", vec![not_null("id", "integer")])
    }
}

fn with_fk(mut table: TableDef) -> TableDef {
    table.foreign_keys.push(fk(Some("orders_customer_fkey"), None, Some("CASCADE")));
    table
}

#[test]
fn foreign_keys_are_dropped_before_their_table_and_added_after_both_tables() {
    // The target has customers referenced by orders; the source has neither: drop the FK, then the tables.
    let target = snapshot("b", vec![customers(), with_fk(orders())], Vec::new());
    let source = snapshot("a", Vec::new(), Vec::new());
    let (steps, _) = planned(&source, &target, None);
    assert_eq!(
        steps,
        ["drop fk orders.orders_customer_fkey", "drop table customers", "drop table orders"]
    );

    // The reverse: both tables are created first, the FK comes last.
    let (steps, warnings) = planned(&target, &source, None);
    assert_eq!(
        steps,
        [
            "create table customers",
            "create table orders",
            "add fk orders.orders_customer_fkey"
        ]
    );
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn only_in_target_items_are_dropped_only_when_selected() {
    let source = snapshot("a", vec![orders()], Vec::new());
    let target = snapshot("b", vec![orders(), table("old", vec![col("x", "int")])], Vec::new());
    let (steps, _) = planned(&source, &target, Some(&[]));
    assert!(steps.is_empty(), "{steps:?}");
    let (steps, _) = planned(&source, &target, Some(&["table:old"]));
    assert_eq!(steps, ["drop table old"]);
}

#[test]
fn a_deselected_dependency_is_a_warning() {
    let source = snapshot("a", vec![customers(), with_fk(orders())], Vec::new());
    let target = snapshot("b", Vec::new(), Vec::new());
    let (steps, warnings) = planned(&source, &target, Some(&["table:orders"]));
    assert_eq!(steps, ["create table orders", "add fk orders.orders_customer_fkey"]);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("needs table customers, which is not selected"), "{warnings:?}");
}

#[test]
fn dependent_views_and_foreign_keys_go_around_a_column_type_change() {
    let mut source_orders = with_fk(orders());
    source_orders.columns[1].type_name = "bigint".into();
    let mut source_customers = customers();
    source_customers.columns[0].type_name = "bigint".into();
    let totals = || ViewDef {
        depends_on: vec!["orders".into()],
        ..view("v_totals", "SELECT customer_id FROM orders")
    };
    let top = || ViewDef {
        depends_on: vec!["v_totals".into()],
        ..view("v_top", "SELECT * FROM v_totals")
    };
    let unrelated = view("v_other", "SELECT 1");
    let source = snapshot("a", vec![source_customers, source_orders], vec![top(), totals(), unrelated.clone()]);
    let target = snapshot("b", vec![customers(), with_fk(orders())], vec![top(), totals(), unrelated]);
    let (steps, _) = planned(&source, &target, None);
    assert_eq!(
        steps,
        [
            "drop fk orders.orders_customer_fkey",
            "drop view v_top",
            "drop view v_totals",
            "alter column customers.id",
            "alter column orders.customer_id",
            "create view v_totals",
            "create view v_top",
            "add fk orders.orders_customer_fkey",
        ]
    );
}

#[test]
fn changed_views_are_recreated_in_dependency_order_with_a_cycle_guard() {
    let dependent = |name: &str, body: &str, on: &[&str]| ViewDef {
        depends_on: on.iter().map(|d| (*d).to_owned()).collect(),
        ..view(name, body)
    };
    let source = snapshot(
        "a",
        Vec::new(),
        vec![
            dependent("v_c", "SELECT * FROM v_b", &["v_b"]),
            dependent("v_b", "SELECT * FROM v_a", &["v_a"]),
            dependent("v_a", "SELECT 2", &[]),
        ],
    );
    let target = snapshot(
        "b",
        Vec::new(),
        vec![
            dependent("v_a", "SELECT 1", &[]),
            dependent("v_b", "SELECT * FROM v_a", &["v_a"]),
            dependent("v_c", "SELECT * FROM v_b", &["v_b"]),
        ],
    );
    let (steps, warnings) = planned(&source, &target, None);
    assert_eq!(
        steps,
        [
            "drop view v_c",
            "drop view v_b",
            "drop view v_a",
            "create view v_a",
            "create view v_b",
            "create view v_c",
        ]
    );
    assert!(warnings.is_empty(), "{warnings:?}");

    let cyclic = snapshot(
        "a",
        Vec::new(),
        vec![
            dependent("v_x", "SELECT * FROM v_y", &["v_y"]),
            dependent("v_y", "SELECT * FROM v_x", &["v_x"]),
        ],
    );
    let (steps, warnings) = planned(&cyclic, &snapshot("b", Vec::new(), Vec::new()), None);
    assert_eq!(steps.len(), 2);
    assert!(warnings.iter().any(|w| w.contains("depend on each other")), "{warnings:?}");
}

#[test]
fn sub_object_changes_drop_before_and_create_after_column_changes() {
    let mut source = orders();
    source.indexes.push(IndexDef {
        name: "orders_status_idx".into(),
        parts: vec![IndexPart::column("status")],
        ..IndexDef::default()
    });
    source.checks.push(CheckDef {
        name: Some("orders_status_check".into()),
        expression: "status <> 'a'".into(),
        system_named: false,
    });
    source.columns.push(col("added", "int"));
    let mut target = orders();
    target.indexes.push(IndexDef {
        name: "orders_status_idx".into(),
        parts: vec![IndexPart::column("note")],
        ..IndexDef::default()
    });
    target.checks.push(CheckDef {
        name: Some("orders_status_check".into()),
        expression: "status <> 'x'".into(),
        system_named: false,
    });
    target.comment = Some("old".into());
    let (steps, _) = planned(
        &snapshot("a", vec![source], Vec::new()),
        &snapshot("b", vec![target], Vec::new()),
        None,
    );
    assert_eq!(
        steps,
        [
            "drop constraint orders.orders_status_check",
            "drop index orders_status_idx",
            "add column orders.added",
            "add constraint orders",
            "create index orders_status_idx",
            "comment orders",
        ]
    );
}

#[test]
fn columns_held_by_dependents_free_them_first_and_recreate_the_dependents() {
    let with = |pk: &str, id_type: &str, status_null: bool, note: bool| {
        let mut table = orders();
        table.primary_key = Some(KeyDef {
            name: Some(pk.into()),
            columns: vec!["id".into()],
            system_named: true,
        });
        table.columns[0].type_name = id_type.into();
        table.columns[2].nullable = status_null;
        table.indexes.push(IndexDef {
            name: "orders_status_idx".into(),
            parts: vec![IndexPart::column("status")],
            ..IndexDef::default()
        });
        table.checks.push(CheckDef {
            name: Some("orders_status_check".into()),
            expression: "[status] <> 'customer_id'".into(),
            system_named: false,
        });
        if note {
            table.columns[3].default = Some("('n')".into());
            table.columns[3].default_name = Some("DF__orders__note__1".into());
            table.uniques.push(KeyDef {
                name: Some("orders_note_key".into()),
                columns: vec!["note".into()],
                system_named: false,
            });
        } else {
            table.columns.remove(3);
        }
        table
    };
    let source = snapshot("a", vec![with("PK__orders__2", "bigint", false, false)], Vec::new());
    let target = snapshot("b", vec![with("PK__orders__1", "integer", true, true)], Vec::new());
    let held = FakeDialect {
        restrict_equals_no_action: false,
        held: true,
        inline: false,
    };
    let (steps, _) = planned_with(&held, &source, &target, None);
    assert_eq!(
        steps,
        [
            "drop constraint orders.orders_note_key",
            "drop constraint orders.PK__orders__1",
            "drop constraint orders.orders_status_check",
            "drop index orders_status_idx",
            "alter column orders.id",
            "alter column orders.status",
            "alter column orders.note",
            "drop column orders.note",
            "add pk orders",
            "add constraint orders",
            "create index orders_status_idx",
        ]
    );
    // Elsewhere the dependents stay where they are.
    let (steps, _) = planned(&source, &target, None);
    assert_eq!(
        steps,
        [
            "drop constraint orders.orders_note_key",
            "alter column orders.id",
            "alter column orders.status",
            "drop column orders.note",
        ]
    );
    assert_eq!(
        super::plan::mentioned("[a] + \"b c\" > 'd''e' AND f(g_1)"),
        ["a", "b c", "AND", "f", "g_1"]
    );
}

#[test]
fn the_script_has_a_header_warnings_and_the_wrapper() {
    let source = snapshot("a", vec![orders()], Vec::new());
    let target = snapshot("b", vec![orders(), table("old", vec![col("x", "int")])], Vec::new());
    let options = CompareOptions::default();
    let script = sync_script(&source, &target, &options, &["table:old".to_owned()], &FAKE, "now");
    assert!(script.sql.contains("at now."));
    assert!(script
        .sql
        .contains("-- Source: shop.a (postgres 17.2)\n-- Target: shop.b (postgres 17.2)"));
    assert!(script.sql.contains("\nBEGIN;\n"), "{}", script.sql);
    assert!(script.transactional);
    // The fake dialect renders steps as comments, which are not counted as statements.
    assert_eq!(script.statements, 0);
    assert_eq!(script.dialect, "postgres");

    let empty = sync_script(&source, &target, &options, &[], &FAKE, "now");
    assert!(empty.sql.contains("Nothing to do"));
    assert!(!empty.transactional);
    assert_eq!(empty.statements, 0);
}

#[test]
fn inline_foreign_keys_go_with_the_table_and_rebuilds_replace_alters() {
    let inline = FakeDialect {
        restrict_equals_no_action: false,
        held: false,
        inline: true,
    };
    // Created tables carry their foreign keys; no separate steps, and no warning for a table created in the same plan.
    let source = snapshot("a", vec![customers(), with_fk(orders())], Vec::new());
    let empty = snapshot("b", Vec::new(), Vec::new());
    let options = CompareOptions::default();
    let diff = super::diff(&source, &empty, &options, &inline);
    let include: Vec<String> = diff.items.iter().map(|item| item.id.clone()).collect();
    let plan = plan(&diff, &include, &source, &empty, &options, &inline);
    let created: Vec<&TableDef> = plan
        .steps
        .iter()
        .filter_map(|p| match &p.step {
            MigrationStep::CreateTable { table } => Some(table),
            _ => None,
        })
        .collect();
    assert_eq!(created.len(), 2);
    assert_eq!(created[1].foreign_keys.len(), 1);
    assert_eq!(plan.steps.len(), 2, "{:?}", plan.steps);
    assert!(plan.warnings.is_empty(), "{:?}", plan.warnings);
    // Dropping the parent leaves the child's key alone; a changed child is rebuilt, and its views go around it.
    let (steps, _) = planned_with(
        &inline,
        &snapshot("a", vec![with_fk(orders())], Vec::new()),
        &snapshot("b", vec![customers(), with_fk(orders())], Vec::new()),
        None,
    );
    assert_eq!(steps, ["drop table customers"]);
    let mut changed = orders();
    changed.columns[2].nullable = !changed.columns[2].nullable;
    let reader = ViewDef {
        depends_on: vec!["orders".into()],
        ..view("v", "SELECT id FROM orders")
    };
    let source = snapshot("a", vec![customers(), changed], vec![reader.clone()]);
    let target = snapshot("b", vec![customers(), with_fk(orders())], vec![reader]);
    let (steps, _) = planned_with(&inline, &source, &target, None);
    assert_eq!(steps.len(), 3, "{steps:?}");
    assert_eq!(steps[0], "drop view v");
    assert!(steps[1].starts_with("RebuildTable"), "{steps:?}");
    assert_eq!(steps[2], "create view v");
}
