//! The Oracle descriptor: EZConnect host/port/service name or a full connect descriptor, the Instant Client folder,
//! capabilities, languages and filter box. Connecting needs Oracle Instant Client on the machine (Fosta Studio offers to
//! download it, see `instant_client.rs`).

use crate::contract::{
    AssistantHints, AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FieldKind,
    FieldSpec, FilterSupport, FormSection, LanguageSpec, RawFilterLang, SqlDialectId, TlsModeId, TlsSupport, UrlSpec,
};

pub const ID: &str = "oracle";
pub const LABEL: &str = "Oracle";

pub fn descriptor() -> EngineDescriptor {
    EngineDescriptor {
        id: ID.into(),
        label: LABEL.into(),
        family: EngineFamily::Sql,
        icon: "database".into(),
        status: EngineStatus::Ready,
        sql_dialect: Some(SqlDialectId::Oracle),
        // Schemas (users) are listed where other engines list databases, so `MultipleDatabases` and not `Schemas`.
        // Sessions have no read-only switch: the gateway's rules block writes in read-only workspaces.
        capabilities: CapabilitySet::of(&[
            Capability::Tree,
            Capability::Describe,
            Capability::MultipleDatabases,
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
            Capability::Explain,
            // Only on 23ai and newer (checked on connect).
            Capability::Vector,
        ]),
        objects_label: "tables".into(),
        connection: ConnectionForm {
            uses_host: true,
            uses_file: false,
            default_port: Some(1521),
            default_user: "system".into(),
            // The service name of the PDB; SIDs are not supported by the core (use a connect descriptor instead).
            database_label: "Service name".into(),
            database_hint: "e.g. FREEPDB1 or ORCLPDB1".into(),
            url: Some(UrlSpec {
                schemes: vec!["oracle".into()],
                example: "oracle://system@localhost:1521/FREEPDB1".into(),
            }),
            auth: vec![AuthMethodId::Password],
            fields: vec![
                FieldSpec::new("options.connectString", FieldKind::Textarea, "Connect descriptor or TNS alias")
                    .placeholder("(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=db)(PORT=1521))(CONNECT_DATA=(SID=ORCL)))")
                    .hint("Used instead of host, port and service name when set.")
                    .section(FormSection::Advanced),
                FieldSpec::new("options.connectTimeout", FieldKind::Number, "Connect timeout (s)")
                    .placeholder("10")
                    .section(FormSection::Advanced),
                FieldSpec::new("options.clientDir", FieldKind::Text, "Instant Client folder")
                    .placeholder("C:\\oracle\\instantclient_23_26")
                    .hint(
                        "A folder with Oracle Instant Client 19c or newer (oci.dll, or libclntsh.so on Linux). Leave it empty to \
                         use PATH, ORACLE_HOME or the client Fosta Studio downloads. The first client loaded stays in use until \
                         the app restarts.",
                    )
                    .section(FormSection::Advanced),
            ],
            ssh: true,
            // TLS (tcps) is verified by Instant Client against its wallet; it cannot skip verification.
            tls: Some(TlsSupport {
                modes: vec![TlsModeId::Disable, TlsModeId::VerifyFull],
                ca: false,
                client_cert: false,
                server_name: false,
                pin: false,
            }),
        },
        languages: vec![LanguageSpec {
            id: "sql".into(),
            label: "SQL".into(),
            editor: "sql".into(),
            dialect: Some(SqlDialectId::Oracle),
            default: true,
            hint: "Oracle SQL and PL/SQL blocks; end a PL/SQL block with '/' on its own line. Each statement commits unless a \
                   transaction is begun. Ctrl+Enter runs the query."
                .into(),
            fences: LanguageSpec::fence_tags(&["sql", "plsql", "oracle"]),
        }],
        filter: FilterSupport {
            structured: true,
            raw: Some(RawFilterLang::Sql),
            placeholder: "Filter: SQL WHERE, e.g. STATUS = 'PAID' AND TOTAL > 10".into(),
        },
        assistant: AssistantHints::sql(LABEL),
    }
}
