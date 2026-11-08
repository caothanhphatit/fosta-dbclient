//! The MongoDB descriptor.

use crate::contract::{
    AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FilterSupport, LanguageSpec,
    RawFilterLang, TlsModeId, TlsSupport, UrlSpec,
};

pub const ID: &str = "mongodb";
pub const LABEL: &str = "MongoDB";

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
    Capability::StatementTimeout,
    Capability::Documents,
    Capability::SchemaGraph,
    Capability::CreateDatabase,
]);

pub fn descriptor() -> EngineDescriptor {
    EngineDescriptor {
        id: ID.into(),
        label: LABEL.into(),
        family: EngineFamily::Document,
        icon: "mongodb".into(),
        status: EngineStatus::Ready,
        sql_dialect: None,
        capabilities: CAPABILITIES,
        objects_label: "collections".into(),
        connection: ConnectionForm {
            uses_host: true,
            uses_file: false,
            default_port: Some(27017),
            default_user: String::new(),
            database_label: "Auth database".into(),
            database_hint: "optional, usually admin".into(),
            url: Some(UrlSpec {
                schemes: vec!["mongodb".into(), "mongodb+srv".into()],
                example: "mongodb+srv://user@cluster0.example.net/?retryWrites=true".into(),
            }),
            auth: vec![AuthMethodId::Password, AuthMethodId::None],
            fields: Vec::new(),
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
            id: "mongo-command".into(),
            label: "Database command".into(),
            editor: "json".into(),
            dialect: None,
            default: true,
            hint: r#"Write a database command as JSON and press Ctrl+Enter, e.g. { "find": "products", "filter": { "stock": 0 }, "limit": 50 }"#
                .into(),
        }],
        filter: FilterSupport {
            structured: true,
            raw: Some(RawFilterLang::MongoJson),
            placeholder: r#"Filter: JSON query, e.g. { "status": "ACTIVE", "stock": { "$gt": 0 } }"#.into(),
        },
    }
}
