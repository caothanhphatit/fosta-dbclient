//! The SQLite descriptor: a file instead of a host, capabilities (the upper bound once the adapter is wired), languages
//! and filter box.

use crate::contract::{
    AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FieldKind, FieldSpec,
    FilterSupport, FormSection, LanguageSpec, RawFilterLang, SqlDialectId, UrlSpec,
};

pub const ID: &str = "sqlite";
pub const LABEL: &str = "SQLite";

pub fn descriptor() -> EngineDescriptor {
    EngineDescriptor {
        id: ID.into(),
        label: LABEL.into(),
        family: EngineFamily::Sql,
        icon: "database".into(),
        status: EngineStatus::Planned,
        sql_dialect: Some(SqlDialectId::Sqlite),
        capabilities: CapabilitySet::of(&[
            Capability::Tree,
            Capability::Describe,
            Capability::BrowseRows,
            Capability::EditRows,
            Capability::RawFilter,
            Capability::KeysetPaging,
            Capability::ExactCount,
            Capability::Query,
            Capability::Cancel,
            Capability::Transactions,
            Capability::ReadOnlySession,
            Capability::StatementTimeout,
            Capability::StreamingResults,
            Capability::SchemaGraph,
            Capability::Explain,
        ]),
        objects_label: "tables".into(),
        connection: ConnectionForm {
            uses_host: false,
            uses_file: true,
            default_port: None,
            default_user: String::new(),
            // The file field below replaces the database field.
            database_label: String::new(),
            database_hint: String::new(),
            url: Some(UrlSpec {
                schemes: vec!["sqlite".into(), "file".into()],
                example: "sqlite:///C:/data/shop.db?mode=ro".into(),
            }),
            auth: vec![AuthMethodId::None],
            fields: vec![
                FieldSpec {
                    required: true,
                    ..FieldSpec::new(
                        "file",
                        FieldKind::File {
                            filters: vec!["db".into(), "sqlite".into(), "sqlite3".into()],
                            save: true,
                        },
                        "Database file",
                    )
                    .placeholder("C:\\data\\shop.db")
                    .hint("A .db, .sqlite or .sqlite3 file, or :memory: for a private in-memory database.")
                },
                FieldSpec::new("options.readOnly", FieldKind::Checkbox, "Open read-only").default_value("false"),
                FieldSpec::new("options.create", FieldKind::Checkbox, "Create the file if it does not exist").default_value("false"),
                FieldSpec::new("options.busyTimeout", FieldKind::Number, "Busy timeout (ms)")
                    .placeholder("5000")
                    .hint("How long to wait for a lock held by another connection.")
                    .section(FormSection::Advanced),
            ],
            ssh: false,
            tls: None,
        },
        languages: vec![LanguageSpec {
            id: "sql".into(),
            label: "SQL".into(),
            editor: "sql".into(),
            dialect: Some(SqlDialectId::Sqlite),
            default: true,
            hint: "SQLite SQL; separate statements with ';'. Ctrl+Enter runs the query.".into(),
        }],
        filter: FilterSupport {
            structured: true,
            raw: Some(RawFilterLang::Sql),
            placeholder: "Filter: SQL WHERE, e.g. status = 'PAID' AND total > 10".into(),
        },
    }
}
