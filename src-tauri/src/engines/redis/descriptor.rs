//! The Redis descriptor (also Valkey and other RESP servers): connection form, capabilities, the redis-cli style console
//! language. The form offers Standalone, Redis Cluster and Sentinel (`options.topology`, `options.nodes`,
//! `options.masterName`, `options.sentinelUser`, the sentinel password in the secondary secret slot,
//! `options.readFromReplicas`, `options.addressMap`).

use crate::contract::{
    AssistantHints, AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FieldKind,
    FieldSpec, FilterSupport, FormSection, LanguageSpec, SecretSlot, TlsModeId, TlsSupport, UrlSpec,
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
                    "redis+sentinel",
                    "rediss+sentinel",
                    "redis-sentinel",
                    "rediss-sentinel",
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
                            ("sentinel".into(), "Sentinel".into()),
                        ],
                    },
                    "Topology",
                )
                .default_value("standalone")
                .part_of_target()
                .hint("Redis Cluster: Host and Port name one seed node; the others are discovered. Sentinel: Host and Port name one sentinel (port 26379 by default)."),
                FieldSpec::new("options.nodes", FieldKind::Text, "More nodes")
                    .part_of_target()
                    .visible_when("options.topology", &["cluster", "sentinel"])
                    .placeholder("10.0.0.2:6379, 10.0.0.3:6379")
                    .hint("Redis Cluster: more seed nodes; Sentinel: more sentinels (host:port, separated by commas), tried when the first is down."),
                // Required only while shown (Sentinel).
                FieldSpec {
                    required: true,
                    ..FieldSpec::new("options.masterName", FieldKind::Text, "Master name")
                        .placeholder("mymaster")
                        .part_of_target()
                        .hint("The name the sentinels monitor (sentinel monitor <name> ...).")
                        .visible_when("options.topology", &["sentinel"])
                },
                FieldSpec::new("options.sentinelUser", FieldKind::Text, "Sentinel user")
                    .part_of_target()
                    .hint("The sentinels' ACL user, when they have one.")
                    .visible_when("options.topology", &["sentinel"])
                    .section(FormSection::Auth),
                FieldSpec::new(
                    "options.sentinelPassword",
                    FieldKind::Password {
                        slot: SecretSlot::SecondaryPassword,
                    },
                    "Sentinel password",
                )
                .hint("The sentinels' own password; left empty, the saved one stays. With none saved, sentinels that ask for a password get the data password.")
                .visible_when("options.topology", &["sentinel"])
                .section(FormSection::Auth),
                FieldSpec::new("options.db", FieldKind::Number, "Database index")
                    .default_value("0")
                    .hint("The logical database (SELECT).")
                    .visible_when("options.topology", &["standalone", "sentinel"]),
                FieldSpec::new("options.readFromReplicas", FieldKind::Checkbox, "Read from replicas")
                    .default_value("false")
                    .hint("Send reads to replicas (they may lag behind their master).")
                    .visible_when("options.topology", &["cluster"])
                    .section(FormSection::Advanced),
                FieldSpec::new("options.addressMap", FieldKind::Text, "Address map")
                    .part_of_target()
                    .placeholder("10.0.1.5:6379=127.0.0.1:17001")
                    .hint("Redis Cluster and Sentinel: dial another address than the one a node or master announces (forwarded ports, NAT).")
                    .visible_when("options.topology", &["cluster", "sentinel"])
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

#[cfg(test)]
mod tests {
    use super::*;

    fn field<'a>(descriptor: &'a EngineDescriptor, key: &str) -> &'a FieldSpec {
        descriptor.connection.fields.iter().find(|field| field.key == key).expect(key)
    }

    #[test]
    fn topology_fields_show_for_their_topology_only() {
        let descriptor = descriptor();
        let shown = |key: &str| {
            field(&descriptor, key)
                .visible_when
                .as_ref()
                .map(|condition| condition.equals.clone())
        };
        let list = |values: &[&str]| Some(values.iter().map(|value| value.to_string()).collect::<Vec<_>>());
        assert_eq!(shown("options.topology"), None);
        assert_eq!(shown("options.nodes"), list(&["cluster", "sentinel"]));
        assert_eq!(shown("options.masterName"), list(&["sentinel"]));
        assert_eq!(shown("options.sentinelUser"), list(&["sentinel"]));
        assert_eq!(shown("options.sentinelPassword"), list(&["sentinel"]));
        assert_eq!(shown("options.db"), list(&["standalone", "sentinel"]));
        assert_eq!(shown("options.readFromReplicas"), list(&["cluster"]));
        assert_eq!(shown("options.addressMap"), list(&["cluster", "sentinel"]));
        assert!(field(&descriptor, "options.masterName").required);
        // Every condition names a select of this form and values it offers.
        for spec in &descriptor.connection.fields {
            let Some(condition) = &spec.visible_when else { continue };
            let FieldKind::Select { options } = &field(&descriptor, &condition.key).kind else {
                panic!("{} depends on a field that is not a select", spec.key);
            };
            for value in &condition.equals {
                assert!(options.iter().any(|(option, _)| option == value), "{}: {value}", spec.key);
            }
        }
    }

    #[test]
    fn options_that_pick_the_servers_are_part_of_the_target() {
        let descriptor = descriptor();
        let marked: Vec<&str> = descriptor
            .connection
            .fields
            .iter()
            .filter(|field| field.part_of_target)
            .map(|field| field.key.as_str())
            .collect();
        assert_eq!(
            marked,
            [
                "options.topology",
                "options.nodes",
                "options.masterName",
                "options.sentinelUser",
                "options.addressMap"
            ]
        );
    }
}
