//! The Redis descriptor (also Valkey and other RESP servers): connection form, capabilities, the redis-cli style console
//! language. The form offers Standalone and Redis Cluster (`options.topology`, `options.nodes`, `options.readFromReplicas`,
//! `options.addressMap`); Sentinel is not supported yet.

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
                schemes: [
                    "redis",
                    "rediss",
                    "valkey",
                    "valkeys",
                    "redis+cluster",
                    "rediss+cluster",
                    "valkey+cluster",
                    "valkeys+cluster",
                ]
                .map(String::from)
                .to_vec(),
                example: "rediss://default@localhost:6379/0".into(),
            }),
            auth: vec![AuthMethodId::Password, AuthMethodId::AclUser, AuthMethodId::None],
            fields: vec![
                FieldSpec::new(
                    "options.topology",
                    FieldKind::Select {
                        options: vec![
                            ("standalone".into(), "Standalone".into()),
                            ("cluster".into(), "Redis Cluster".into()),
                        ],
                    },
                    "Topology",
                )
                .default_value("standalone")
                .hint("Redis Cluster: Host and Port name one seed node; the others are discovered."),
                FieldSpec::new("options.nodes", FieldKind::Text, "More nodes")
                    .placeholder("10.0.0.2:6379, 10.0.0.3:6379")
                    .hint("Redis Cluster: more seed nodes (host:port, separated by commas), tried when the first is down."),
                FieldSpec::new("options.db", FieldKind::Number, "Database index")
                    .default_value("0")
                    .hint("The logical database (SELECT); Redis Cluster has database 0 only."),
                FieldSpec::new("options.readFromReplicas", FieldKind::Checkbox, "Read from replicas")
                    .default_value("false")
                    .hint("Redis Cluster: send reads to replicas (they may lag behind their master).")
                    .section(FormSection::Advanced),
                FieldSpec::new("options.addressMap", FieldKind::Text, "Address map")
                    .placeholder("10.0.1.5:6379=127.0.0.1:17001")
                    .hint("Redis Cluster: dial another address than the one a node announces (forwarded ports, NAT).")
                    .section(FormSection::Advanced),
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
