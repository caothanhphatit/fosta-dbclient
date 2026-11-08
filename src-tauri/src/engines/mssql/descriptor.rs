//! The SQL Server descriptor.

use crate::contract::{
    AssistantHints, AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FilterSupport,
    LanguageSpec, RawFilterLang, SqlDialectId, TlsModeId, TlsSupport, UrlSpec,
};

pub const ID: &str = "mssql";
pub const LABEL: &str = "SQL Server";

/// SQL Server has no read-only session mode, so `ReadOnlySession` is not offered.
pub const CAPABILITIES: CapabilitySet = CapabilitySet::of(&[
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
    Capability::StatementTimeout,
    Capability::StreamingResults,
    Capability::SchemaGraph,
    Capability::CreateDatabase,
    Capability::Explain,
]);

pub fn descriptor() -> EngineDescriptor {
    EngineDescriptor {
        id: ID.into(),
        label: LABEL.into(),
        family: EngineFamily::Sql,
        icon: "mssql".into(),
        status: EngineStatus::Ready,
        sql_dialect: Some(SqlDialectId::MsSql),
        capabilities: CAPABILITIES,
        objects_label: "tables".into(),
        connection: ConnectionForm {
            uses_host: true,
            uses_file: false,
            default_port: Some(1433),
            default_user: "sa".into(),
            database_label: "Database".into(),
            database_hint: "optional, defaults to the login's database".into(),
            url: Some(UrlSpec {
                schemes: vec!["sqlserver".into(), "mssql".into(), "ado".into()],
                example: "Server=localhost,1433;Database=master;User Id=sa;Encrypt=true;TrustServerCertificate=true".into(),
            }),
            auth: vec![AuthMethodId::Password],
            fields: Vec::new(),
            ssh: true,
            // Encrypt off = disable, on = require; "Trust server certificate" = accept invalid certificates.
            tls: Some(TlsSupport {
                modes: vec![TlsModeId::Disable, TlsModeId::Require],
                ca: false,
                client_cert: false,
                server_name: false,
                pin: false,
            }),
        },
        languages: vec![LanguageSpec {
            id: "sql".into(),
            label: "T-SQL".into(),
            editor: "sql".into(),
            dialect: Some(SqlDialectId::MsSql),
            default: true,
            hint: "Ctrl+Enter runs the batch. Tables are addressed as schema.table; the default schema is dbo.".into(),
            fences: LanguageSpec::fence_tags(&["sql", "tsql", "mssql"]),
        }],
        filter: FilterSupport {
            structured: true,
            raw: Some(RawFilterLang::Sql),
            placeholder: "Filter: T-SQL WHERE, e.g. Status = 'Active'".into(),
        },
        assistant: AssistantHints::sql(LABEL),
    }
}
