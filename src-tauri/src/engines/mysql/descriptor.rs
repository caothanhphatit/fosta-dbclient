//! MariaDB and MySQL descriptors: one module, two engine ids.

use crate::contract::{
    AssistantHints, AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FilterSupport,
    LanguageSpec, RawFilterLang, SqlDialectId, TlsModeId, TlsSupport, UrlSpec,
};

/// Which server the engine id stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    MariaDb,
    MySql,
}

impl Flavor {
    pub fn id(self) -> &'static str {
        match self {
            Flavor::MariaDb => "mariadb",
            Flavor::MySql => "mysql",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Flavor::MariaDb => "MariaDB",
            Flavor::MySql => "MySQL",
        }
    }

    pub fn dialect(self) -> SqlDialectId {
        match self {
            Flavor::MariaDb => SqlDialectId::MariaDb,
            Flavor::MySql => SqlDialectId::MySql,
        }
    }
}

/// Everything MariaDB/MySQL can do once connected; `Vector` only on MariaDB 11.7+ (the connection drops it otherwise).
pub const CAPABILITIES: CapabilitySet = CapabilitySet::of(&[
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
    Capability::ReadOnlySession,
    Capability::StatementTimeout,
    Capability::StreamingResults,
    Capability::SchemaGraph,
    Capability::CreateDatabase,
    Capability::Explain,
    Capability::Vector,
]);

pub fn descriptor(flavor: Flavor) -> EngineDescriptor {
    let capabilities = match flavor {
        Flavor::MariaDb => CAPABILITIES,
        Flavor::MySql => CAPABILITIES.without(Capability::Vector),
    };
    EngineDescriptor {
        id: flavor.id().into(),
        label: flavor.label().into(),
        family: EngineFamily::Sql,
        icon: flavor.id().into(),
        status: EngineStatus::Ready,
        sql_dialect: Some(flavor.dialect()),
        capabilities,
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
                example: format!("{}://root@localhost:3306/shop", flavor.id()),
            }),
            auth: vec![AuthMethodId::Password],
            fields: Vec::new(),
            ssh: true,
            tls: Some(TlsSupport {
                modes: vec![
                    TlsModeId::Disable,
                    TlsModeId::Prefer,
                    TlsModeId::Require,
                    TlsModeId::VerifyCa,
                    TlsModeId::VerifyFull,
                ],
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
            dialect: Some(flavor.dialect()),
            default: true,
            hint: "Ctrl+Enter runs the query. Selected text runs on its own.".into(),
            fences: match flavor {
                Flavor::MariaDb => LanguageSpec::fence_tags(&["sql", "mariadb", "mysql"]),
                Flavor::MySql => LanguageSpec::fence_tags(&["sql", "mysql"]),
            },
        }],
        filter: FilterSupport {
            structured: true,
            raw: Some(RawFilterLang::Sql),
            placeholder: "Filter: SQL WHERE, e.g. status = 'ACTIVE'".into(),
        },
        // MySQL stands for both as a translation target.
        assistant: match flavor {
            Flavor::MariaDb => AssistantHints::sql(flavor.label()).not_a_target(),
            Flavor::MySql => AssistantHints::sql(flavor.label()),
        },
    }
}
