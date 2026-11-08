//! The PostgreSQL descriptor: connection form, capabilities (the upper bound), languages and
//! filter box.

use crate::contract::{
    AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FieldKind, FieldSpec,
    FilterSupport, FormSection, LanguageSpec, RawFilterLang, SqlDialectId, TlsModeId, TlsSupport, UrlSpec,
};

pub const ID: &str = "postgres";
pub const LABEL: &str = "PostgreSQL";

pub fn descriptor() -> EngineDescriptor {
    EngineDescriptor {
        id: ID.into(),
        label: LABEL.into(),
        family: EngineFamily::Sql,
        icon: "database".into(),
        status: EngineStatus::Ready,
        sql_dialect: Some(SqlDialectId::Postgres),
        capabilities: CapabilitySet::of(&[
            Capability::Tree,
            Capability::Describe,
            Capability::MultipleDatabases,
            Capability::Schemas,
            Capability::BrowseRows,
            Capability::EditRows,
            Capability::RawFilter,
            Capability::KeysetPaging,
            Capability::EstimatedCount,
            Capability::ExactCount,
            Capability::Query,
            Capability::Cancel,
            Capability::Transactions,
            Capability::ReadOnlySession,
            Capability::StatementTimeout,
            Capability::StreamingResults,
            Capability::SchemaGraph,
            Capability::CreateDatabase,
            Capability::Explain,
        ]),
        objects_label: "tables".into(),
        connection: ConnectionForm {
            uses_host: true,
            uses_file: false,
            default_port: Some(5432),
            default_user: "postgres".into(),
            database_label: "Database".into(),
            database_hint: "postgres when empty".into(),
            url: Some(UrlSpec {
                schemes: vec!["postgres".into(), "postgresql".into()],
                example: "postgres://postgres@localhost:5432/postgres?sslmode=require".into(),
            }),
            auth: vec![AuthMethodId::Password],
            fields: vec![
                FieldSpec::new("options.applicationName", FieldKind::Text, "Application name")
                    .placeholder("Fosta Studio")
                    .hint("Shown in pg_stat_activity.")
                    .section(FormSection::Advanced),
                FieldSpec::new("options.connectTimeout", FieldKind::Number, "Connect timeout (s)")
                    .placeholder("10")
                    .section(FormSection::Advanced),
            ],
            ssh: true,
            // All of it goes through `net::tls::rustls_client_config`: a CA file, a client certificate, a server name
            // (through an SSH tunnel the manager sets it to the database host) and a certificate pin.
            tls: Some(TlsSupport {
                modes: vec![TlsModeId::Disable, TlsModeId::Require, TlsModeId::VerifyCa, TlsModeId::VerifyFull],
                ca: true,
                client_cert: true,
                server_name: true,
                pin: true,
            }),
        },
        languages: vec![LanguageSpec {
            id: "sql".into(),
            label: "SQL".into(),
            editor: "sql".into(),
            dialect: Some(SqlDialectId::Postgres),
            default: true,
            hint: "PostgreSQL SQL; separate statements with ';'. Ctrl+Enter runs the query.".into(),
        }],
        filter: FilterSupport {
            structured: true,
            raw: Some(RawFilterLang::Sql),
            placeholder: "Filter: SQL WHERE, e.g. status = 'PAID' AND total > 10".into(),
        },
    }
}
