//! The Redis descriptor (also Valkey and other RESP servers): connection form, capabilities, the redis-cli style console
//! language. Cluster and Sentinel are out of scope: the form has no fields for them.

use crate::contract::{
    AssistantHints, AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FieldKind,
    FieldSpec, FilterSupport, FormSection, LanguageSpec, TlsModeId, TlsSupport, UrlSpec,
};

use super::connection::DEFAULT_PORT;

pub const ID: &str = "redis";
pub const LABEL: &str = "Redis";

pub fn descriptor() -> EngineDescriptor {
    EngineDescriptor {
        id: ID.into(),
        label: LABEL.into(),
        family: EngineFamily::KeyValue,
        icon: "redis".into(),
        status: EngineStatus::Ready,
        sql_dialect: None,
        // No row browsing or editing (`tabular`): keys are read and written through the typed `redis.*` operations.
        capabilities: CapabilitySet::of(&[
            Capability::Tree,
            Capability::Describe,
            Capability::MultipleDatabases,
            Capability::Query,
            Capability::Cancel,
            Capability::StreamingResults,
            Capability::KeyValue,
            Capability::Console,
            Capability::ServerOverview,
            // Vector fields of search indexes (FT.*), only when the server has the Query Engine (checked on connect).
            Capability::Vector,
        ]),
        objects_label: "keys".into(),
        connection: ConnectionForm {
            uses_host: true,
            uses_file: false,
            default_port: Some(DEFAULT_PORT),
            // Empty means the `default` ACL user.
            default_user: String::new(),
            // The logical database is a number: see `options.db`.
            database_label: String::new(),
            database_hint: String::new(),
            url: Some(UrlSpec {
                schemes: vec!["redis".into(), "rediss".into(), "valkey".into(), "valkeys".into()],
                example: "rediss://default@localhost:6379/0".into(),
            }),
            auth: vec![AuthMethodId::Password, AuthMethodId::AclUser, AuthMethodId::None],
            fields: vec![
                FieldSpec::new("options.db", FieldKind::Number, "Database index")
                    .default_value("0")
                    .hint("The logical database (SELECT); ignored by Redis Cluster."),
                FieldSpec::new("options.resp3", FieldKind::Checkbox, "Use RESP3")
                    .default_value("false")
                    .hint("Typed maps, sets and doubles in replies (Redis 6+).")
                    .section(FormSection::Advanced),
                FieldSpec::new("options.commandTimeoutMs", FieldKind::Number, "Command timeout (ms)")
                    .placeholder("30000")
                    .section(FormSection::Advanced),
                FieldSpec::new("options.clientName", FieldKind::Text, "Client name")
                    .placeholder("Fosta Studio")
                    .hint("Sent as CLIENT SETNAME, shown in CLIENT LIST.")
                    .section(FormSection::Advanced),
            ],
            ssh: true,
            // rediss://. verifyFull = chain and host name, verifyCa = chain only, require = any certificate.
            tls: Some(TlsSupport {
                modes: vec![TlsModeId::Disable, TlsModeId::Require, TlsModeId::VerifyCa, TlsModeId::VerifyFull],
                ca: true,
                client_cert: true,
                server_name: false,
                pin: false,
            }),
        },
        languages: vec![LanguageSpec {
            id: "redis".into(),
            label: "Redis".into(),
            editor: "redis".into(),
            dialect: None,
            default: true,
            hint: "One command per line, as in redis-cli; # starts a comment. Ctrl+Enter runs the commands.".into(),
            fences: LanguageSpec::fence_tags(&["redis"]),
        }],
        filter: FilterSupport {
            structured: false,
            raw: None,
            placeholder: "Key pattern, e.g. user:*".into(),
        },
        assistant: AssistantHints {
            test_data: "Redis commands (SET, HSET, RPUSH, …)".into(),
            ..AssistantHints::default()
        },
    }
}
