//! The ClickHouse descriptor: the HTTP interface (8123, or 8443 over HTTPS), capabilities (the upper bound once the
//! adapter is wired; no transactions), languages and filter box.

use crate::contract::{
    AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FieldKind, FieldSpec,
    FilterSupport, FormSection, LanguageSpec, RawFilterLang, SqlDialectId, TlsModeId, TlsSupport, UrlSpec,
};

pub const ID: &str = "clickhouse";
pub const LABEL: &str = "ClickHouse";

pub fn descriptor() -> EngineDescriptor {
    EngineDescriptor {
        id: ID.into(),
        label: LABEL.into(),
        family: EngineFamily::Sql,
        icon: "database".into(),
        status: EngineStatus::Ready,
        sql_dialect: Some(SqlDialectId::ClickHouse),
        capabilities: CapabilitySet::of(&[
            Capability::Tree,
            Capability::Describe,
            Capability::MultipleDatabases,
            Capability::BrowseRows,
            // Row edits are asynchronous mutations (MergeTree tables only), applied one by one: see edit.rs.
            Capability::EditRows,
            Capability::RawFilter,
            Capability::KeysetPaging,
            Capability::EstimatedCount,
            Capability::ExactCount,
            Capability::Query,
            Capability::Cancel,
            Capability::ReadOnlySession,
            Capability::StatementTimeout,
            Capability::StreamingResults,
            Capability::CreateDatabase,
            Capability::Explain,
            // Parts, mutations and the query log (`clickhouse.*` operations).
            Capability::ServerOverview,
        ]),
        objects_label: "tables".into(),
        connection: ConnectionForm {
            uses_host: true,
            uses_file: false,
            default_port: Some(8123),
            default_user: "default".into(),
            database_label: "Database".into(),
            database_hint: "default when empty".into(),
            url: Some(UrlSpec {
                schemes: vec!["clickhouse".into(), "ch".into(), "http".into(), "https".into()],
                example: "clickhouse://default@localhost:8123/default?secure=true".into(),
            }),
            auth: vec![AuthMethodId::Password],
            fields: vec![FieldSpec::new("options.connectTimeout", FieldKind::Number, "Connect timeout (s)")
                .placeholder("10")
                .section(FormSection::Advanced)],
            ssh: true,
            // TLS means HTTPS (port 8443 by default); `require` skips certificate verification.
            tls: Some(TlsSupport {
                modes: vec![TlsModeId::Disable, TlsModeId::Require, TlsModeId::VerifyFull],
                ca: true,
                client_cert: false,
                server_name: false,
                pin: false,
            }),
        },
        languages: vec![LanguageSpec {
            id: "sql".into(),
            label: "SQL".into(),
            editor: "sql".into(),
            dialect: Some(SqlDialectId::ClickHouse),
            default: true,
            hint: "ClickHouse SQL; separate statements with ';'. Ctrl+Enter runs the query.".into(),
        }],
        filter: FilterSupport {
            structured: true,
            raw: Some(RawFilterLang::Sql),
            placeholder: "Filter: SQL WHERE, e.g. event_date >= today() - 7".into(),
        },
    }
}
