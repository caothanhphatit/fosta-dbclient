//! Sample JSON of every contract type, checked in under `src/core/__fixtures__/contract/`. The UI's vitest
//! (`src/core/contract.test.ts`) checks them against the TypeScript mirrors in `src/core/contract.ts`, so a change on
//! either side that the other does not follow fails a test.
//!
//! When a contract type changes on purpose, regenerate with `FOSTA_UPDATE_FIXTURES=1 cargo test contract::fixtures`.

use std::path::PathBuf;

use chrono::{NaiveDate, NaiveTime};
use serde::Serialize;
use serde_json::json;

use super::*;
use crate::contract::{
    descriptor::{FieldKind, TlsSupport},
    errors::CONFLICT,
    object::BuiltinAction,
    tabular::conflict_error,
};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/core/__fixtures__/contract")
}

fn sample_values() -> Vec<Value> {
    let date = NaiveDate::from_ymd_opt(2026, 11, 8).unwrap();
    let time = NaiveTime::from_hms_milli_opt(9, 30, 0, 250).unwrap();
    vec![
        Value::Null,
        Value::Bool(true),
        Value::Int(42),
        Value::Int(i64::MAX),
        Value::Float(1.5),
        Value::Float(f64::NAN),
        Value::Decimal("12.50".into()),
        Value::Text("PAID".into()),
        Value::Bytes(vec![0xde, 0xad]),
        Value::Date(date),
        Value::Time(time),
        Value::Timestamp(date.and_time(time)),
        Value::TimestampTz(date.and_time(time).and_utc().fixed_offset()),
        Value::Interval("1 day".into()),
        Value::Uuid("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".into()),
        Value::Json(r#"{"a":1}"#.into()),
        Value::Array(vec![Value::Int(1), Value::Null]),
        Value::ObjectId("65f0c0ffee0000000000abcd".into()),
        Value::Document(vec![("city".into(), Value::Text("Hanoi".into()))]),
        Value::Other {
            type_name: "inet".into(),
            text: "10.0.0.1".into(),
        },
    ]
}

fn orders() -> ObjectRef {
    ObjectRef::table("shop", None, "orders")
}

fn column(name: &str, type_name: &str, kind: ValueKind, primary: bool) -> ColumnInfo {
    let mut column = ColumnInfo::result(name, type_name, kind);
    column.nullable = !primary;
    column.hints.primary = primary;
    column
}

fn descriptor() -> EngineDescriptor {
    EngineDescriptor {
        id: "mariadb".into(),
        label: "MariaDB".into(),
        family: EngineFamily::Sql,
        icon: "mariadb".into(),
        status: EngineStatus::Ready,
        sql_dialect: Some(SqlDialectId::MariaDb),
        capabilities: CapabilitySet::of(&[Capability::Tree, Capability::BrowseRows, Capability::EditRows, Capability::Query]),
        objects_label: "tables".into(),
        connection: ConnectionForm {
            uses_host: true,
            uses_file: false,
            default_port: Some(3306),
            default_user: "root".into(),
            database_label: "Default database".into(),
            database_hint: "optional".into(),
            url: Some(UrlSpec {
                schemes: vec!["mariadb".into(), "mysql".into()],
                example: "mariadb://root@localhost:3306/shop".into(),
            }),
            auth: vec![AuthMethodId::Password],
            fields: vec![
                FieldSpec::new("options.charset", FieldKind::Text, "Character set").placeholder("utf8mb4"),
                FieldSpec::new(
                    "options.sslMode",
                    FieldKind::Select {
                        options: vec![("prefer".into(), "Prefer".into()), ("require".into(), "Require".into())],
                    },
                    "TLS",
                )
                .section(FormSection::Advanced),
                FieldSpec::new("options.compress", FieldKind::Checkbox, "Compress").default_value("false"),
            ],
            ssh: true,
            tls: Some(TlsSupport {
                modes: vec![TlsModeId::Disable, TlsModeId::Require, TlsModeId::VerifyFull],
                ca: true,
                client_cert: true,
                server_name: false,
                pin: false,
            }),
        },
        languages: vec![LanguageSpec {
            id: "sql".into(),
            label: "SQL".into(),
            editor: "sql".into(),
            dialect: Some(SqlDialectId::MariaDb),
            default: true,
            hint: "Ctrl+Enter runs the query.".into(),
            fences: LanguageSpec::fence_tags(&["sql", "mariadb"]),
        }],
        filter: FilterSupport {
            structured: true,
            raw: Some(RawFilterLang::Sql),
            placeholder: "Filter: SQL WHERE, e.g. status = 'PAID'".into(),
        },
        assistant: crate::contract::AssistantHints {
            prompts: vec![crate::contract::AssistantPrompt {
                label: "Window functions".into(),
                icon: "layers".into(),
                prompt: "Write a query with a window function that ".into(),
            }],
            translate_label: Some("MariaDB".into()),
            ..crate::contract::AssistantHints::sql("MariaDB")
        },
    }
}

fn fixtures() -> Vec<(&'static str, serde_json::Value)> {
    fn v(value: impl Serialize) -> serde_json::Value {
        serde_json::to_value(value).unwrap()
    }
    let table_node = ObjectNode {
        key: NodeKey {
            id: "db:shop/folder:tables/table:orders".into(),
            kind: NodeKind::TABLE,
            object: Some(orders()),
        },
        label: "orders".into(),
        kind: NodeKind::TABLE,
        icon: "table".into(),
        object: Some(orders()),
        detail: Some("~8k rows".into()),
        badges: vec![Badge {
            text: "system".into(),
            tone: Tone::Neutral,
        }],
        expandable: false,
        default_view: Some("data".into()),
        actions: vec![
            NodeAction::view("design", "Design", "design"),
            NodeAction::query("truncate", "Truncate…", "TRUNCATE TABLE {qualified}")
                .danger()
                .separated(),
            NodeAction::builtin("copy", "Copy name", BuiltinAction::CopyName),
            NodeAction::new(
                "info",
                "Server info",
                ActionTarget::Capability {
                    name: "redis.info".into(),
                    args: json!({ "section": "server" }),
                },
            ),
        ],
    };
    let mut description = ObjectDescription::new(orders());
    description.columns = vec![
        column("id", "int(11)", ValueKind::Int, true),
        column("status", "varchar(20)", ValueKind::Text, false),
        column("tags", "json", ValueKind::Array(Box::new(ValueKind::Text)), false),
    ];
    description.primary_key = Some(KeyInfo {
        name: Some("PRIMARY".into()),
        columns: vec!["id".into()],
    });
    description.foreign_keys = vec![ForeignKeyInfo {
        name: Some("fk_customer".into()),
        columns: vec!["customer_id".into()],
        ref_database: None,
        ref_schema: None,
        ref_table: "customers".into(),
        ref_columns: vec!["id".into()],
        on_update: None,
        on_delete: Some("CASCADE".into()),
    }];
    description.referenced_by = vec![ReferencingKey {
        name: Some("fk_order".into()),
        database: Some("shop".into()),
        schema: None,
        table: "order_items".into(),
        columns: vec!["order_id".into()],
        ref_columns: vec!["id".into()],
    }];
    description.indexes = vec![IndexInfo {
        name: "PRIMARY".into(),
        columns: vec!["id".into()],
        unique: true,
        primary: true,
        method: Some("BTREE".into()),
        definition: None,
    }];
    description.row_estimate = Some(8000);
    description.ddl = Some("CREATE TABLE orders (…)".into());
    description.extra = vec![("engine".into(), "InnoDB".into())];

    let browse = BrowseRequest {
        filter: Filter::Conditions {
            conditions: vec![FilterCond::new("status", FilterOp::Eq, &["PAID"])],
            match_any: false,
        },
        sort: vec![SortKey::asc("id")],
        estimate_total: true,
        ..BrowseRequest::new(orders(), 200)
    };
    let page = Page {
        columns: description.columns.clone(),
        rows: vec![vec![Value::Int(1), Value::Text("PAID".into()), Value::Null]],
        has_more: true,
        key_columns: vec!["id".into()],
        next_keyset: Some(vec![Value::Int(1)]),
        total: Some(Total { value: 8000, exact: false }),
        statement: Some("SELECT … WHERE `status` = ? ORDER BY `id` LIMIT 200".into()),
        elapsed_ms: 4,
        row_versions: Some(vec![Value::Bytes(vec![0, 0, 0, 0, 0, 0, 7, 209])]),
    };
    let key = vec![Field::new("id", Value::Int(1))];
    let update = RowChange::Update {
        key: key.clone(),
        values: vec![Field::new("status", Value::Text("SHIPPED".into()))],
        original: vec![Field::new("status", Value::Text("PAID".into()))],
        version: None,
        force: false,
    };
    let changes = RowChangeSet {
        object: orders(),
        changes: vec![
            RowChange::Insert {
                values: vec![Field::new("status", Value::Text("NEW".into()))],
            },
            update.clone(),
            RowChange::Delete {
                key: vec![Field::new("id", Value::Int(2))],
                original: vec![Field::new("status", Value::Text("PAID".into()))],
                version: Some(Value::Bytes(vec![0, 0, 0, 0, 0, 0, 7, 210])),
                force: true,
            },
        ],
    };
    let conflict = conflict_error(
        &orders(),
        vec![
            RowConflict::new(
                1,
                &update,
                Some(vec![key[0].clone(), Field::new("status", Value::Text("CANCELLED".into()))]),
            ),
            RowConflict::new(2, &changes.changes[2], None),
        ],
    );
    assert_eq!(conflict.code(), Some(CONFLICT));
    let events = vec![
        StreamEvent::Statement {
            index: 0,
            sql: "SELECT 1".into(),
        },
        StreamEvent::Columns {
            index: 0,
            columns: vec![ColumnInfo::result("1", "bigint", ValueKind::Int)],
        },
        StreamEvent::Rows {
            index: 0,
            rows: vec![vec![Value::Int(1)]],
        },
        StreamEvent::Done {
            index: 0,
            affected: None,
            rows: 1,
            truncated: false,
            elapsed_ms: 2,
        },
        StreamEvent::Notice {
            index: 0,
            text: "1 row affected".into(),
        },
    ];
    let state = TransactionState::from(TxState::Active);
    vec![
        ("values", v(sample_values())),
        (
            "tree-request",
            v(TreeRequest {
                parent: Some(table_node.key.clone()),
                cursor: Some("c2".into()),
                limit: Some(500),
                filter: None,
            }),
        ),
        (
            "tree-children",
            v(TreeChildren {
                nodes: vec![table_node],
                next_cursor: Some("c3".into()),
                total_hint: Some(1200),
            }),
        ),
        ("object-description", v(description)),
        ("browse-request", v(browse)),
        ("page", v(page)),
        ("row-change-set", v(&changes)),
        (
            "apply-result",
            v(ApplyResult {
                inserted: 1,
                updated: 1,
                deleted: 1,
            }),
        ),
        ("conflict-error", v(conflict)),
        ("stream-events", v(events)),
        (
            "run-request",
            v(RunRequest {
                text: "SELECT 1".into(),
                database: Some("shop".into()),
                offset: 0,
            }),
        ),
        (
            "run-summary",
            v(RunSummary {
                statements: 1,
                elapsed_ms: 3,
                stopped: false,
                transaction: state,
            }),
        ),
        (
            "session-info",
            v(SessionInfo {
                session_id: "s1".into(),
                language: "sql".into(),
                transaction: state,
                cancellable: true,
            }),
        ),
        (
            "session-options",
            v(SessionOptions {
                language: Some("sql".into()),
                database: None,
            }),
        ),
        ("engine-descriptor", v(descriptor())),
        (
            "connect-info",
            v(ConnectInfo {
                engine: "mariadb".into(),
                server: ServerInfo {
                    product: "MariaDB".into(),
                    version: "11.8.2-MariaDB".into(),
                    label: "MariaDB 11.8.2".into(),
                    flavor: None,
                },
                capabilities: descriptor().capabilities,
                languages: descriptor().languages,
                connection_actions: vec![NodeAction::builtin("refresh", "Refresh", BuiltinAction::Refresh)],
            }),
        ),
        (
            "connection-spec",
            v(ConnectionSpec {
                engine: "mssql".into(),
                host: "localhost".into(),
                port: 1433,
                user: "sa".into(),
                database: Some("master".into()),
                tls: Some(TlsConfig {
                    mode: TlsMode::Require,
                    accept_invalid_certs: true,
                    ..TlsConfig::default()
                }),
                ..ConnectionSpec::default()
            }),
        ),
    ]
}

#[test]
fn contract_fixtures_match_the_checked_in_json() {
    let update = std::env::var("FOSTA_UPDATE_FIXTURES").is_ok_and(|v| v == "1");
    let dir = dir();
    let mut stale = Vec::new();
    for (name, value) in fixtures() {
        let path = dir.join(format!("{name}.json"));
        let text = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
        if update {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, &text).unwrap();
        } else if std::fs::read_to_string(&path).map(|old| old.replace("\r\n", "\n")).ok().as_deref() != Some(text.as_str()) {
            stale.push(name);
        }
    }
    assert!(
        stale.is_empty(),
        "contract fixtures changed: {stale:?}. If intended, run FOSTA_UPDATE_FIXTURES=1 cargo test contract::fixtures and update src/core/contract.ts"
    );
}

#[test]
fn fixtures_deserialize_back_into_the_contract_types() {
    let get = |name: &str| fixtures().into_iter().find(|(n, _)| *n == name).unwrap().1;
    let _: Vec<Value> = serde_json::from_value(get("values")).unwrap();
    let _: TreeChildren = serde_json::from_value(get("tree-children")).unwrap();
    let _: ObjectDescription = serde_json::from_value(get("object-description")).unwrap();
    let _: BrowseRequest = serde_json::from_value(get("browse-request")).unwrap();
    let _: Page = serde_json::from_value(get("page")).unwrap();
    let back: RowChangeSet = serde_json::from_value(get("row-change-set")).unwrap();
    assert_eq!(back.changes.len(), 3);
    let _: Vec<StreamEvent> = serde_json::from_value(get("stream-events")).unwrap();
    let _: EngineDescriptor = serde_json::from_value(get("engine-descriptor")).unwrap();
    let _: ConnectInfo = serde_json::from_value(get("connect-info")).unwrap();
    let _: ConnectionSpec = serde_json::from_value(get("connection-spec")).unwrap();
}
