//! The Elasticsearch and OpenSearch descriptors: one module, two engines. They share the form, capabilities and the
//! Dev Tools console; Elasticsearch adds ES|QL and the Cloud ID, OpenSearch has neither.

use crate::contract::{
    AssistantHints, AuthMethodId, Capability, CapabilitySet, ConnectionForm, EngineDescriptor, EngineFamily, EngineStatus, FieldKind,
    FieldSpec, FilterSupport, FormSection, LanguageSpec, RawFilterLang, TlsModeId, TlsSupport, UrlSpec,
};

/// Which product an engine id stands for (the server's own distribution is detected when connecting).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Elasticsearch,
    OpenSearch,
}

impl Flavor {
    pub fn id(self) -> &'static str {
        match self {
            Flavor::Elasticsearch => "elasticsearch",
            Flavor::OpenSearch => "opensearch",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Flavor::Elasticsearch => "Elasticsearch",
            Flavor::OpenSearch => "OpenSearch",
        }
    }

    /// The SQL endpoint (`_plugins/_sql` on OpenSearch).
    pub fn sql_path(self) -> &'static str {
        match self {
            Flavor::Elasticsearch => "/_sql",
            Flavor::OpenSearch => "/_plugins/_sql",
        }
    }
}

pub const DEFAULT_PORT: u16 = 9200;

/// The Elasticsearch descriptor.
pub fn descriptor() -> EngineDescriptor {
    build(Flavor::Elasticsearch)
}

/// The OpenSearch descriptor.
pub fn opensearch_descriptor() -> EngineDescriptor {
    build(Flavor::OpenSearch)
}

fn build(flavor: Flavor) -> EngineDescriptor {
    let elastic = flavor == Flavor::Elasticsearch;
    let mut fields = vec![FieldSpec::new("url", FieldKind::Textarea, "Node URLs")
        .placeholder("https://es1:9200, https://es2:9200")
        .hint("Several nodes separated by commas; later ones are used when the first does not answer. Replaces host and port.")
        .section(FormSection::Advanced)];
    if elastic {
        fields.insert(
            0,
            FieldSpec::new("options.cloudId", FieldKind::Text, "Cloud ID")
                .placeholder("my-deployment:ZXUtY2VudHJhbC0xLmF3cy5jbG91ZC5lcy5pbyQ...")
                .hint("From the Elastic Cloud deployment page; replaces host and port."),
        );
    }
    fields.push(
        FieldSpec::new("options.requestTimeoutMs", FieldKind::Number, "Request timeout (ms)")
            .placeholder("30000")
            .section(FormSection::Advanced),
    );

    let mut languages = vec![LanguageSpec {
        id: "es-console".into(),
        label: "Console".into(),
        editor: "es-console".into(),
        dialect: None,
        default: true,
        hint: "Dev Tools format: METHOD /path on one line, then a JSON body. Ctrl+Enter runs the requests.".into(),
        fences: LanguageSpec::fence_tags(&["http", "es", "console", "elasticsearch", "opensearch"]),
    }];
    if elastic {
        languages.push(LanguageSpec {
            id: "esql".into(),
            label: "ES|QL".into(),
            editor: "sql".into(),
            dialect: None,
            default: false,
            hint: "One ES|QL query, e.g. FROM logs-* | WHERE status >= 500 | LIMIT 100. Ctrl+Enter runs it.".into(),
            fences: LanguageSpec::fence_tags(&["esql"]),
        });
    }
    languages.push(LanguageSpec {
        id: "es-sql".into(),
        label: "SQL".into(),
        editor: "sql".into(),
        dialect: None,
        default: false,
        hint: if elastic {
            "One Elasticsearch SQL query, e.g. SELECT * FROM \"logs-*\" LIMIT 100. Ctrl+Enter runs it.".into()
        } else {
            "One query for the OpenSearch SQL plugin, e.g. SELECT * FROM logs LIMIT 100. Ctrl+Enter runs it.".into()
        },
        fences: LanguageSpec::fence_tags(&["sql"]),
    });

    let mut auth = vec![AuthMethodId::Password];
    if elastic {
        auth.push(AuthMethodId::ApiKey);
    }
    auth.extend([AuthMethodId::Bearer, AuthMethodId::None]);

    EngineDescriptor {
        id: flavor.id().into(),
        label: flavor.label().into(),
        family: EngineFamily::Search,
        icon: "search".into(),
        status: EngineStatus::Ready,
        sql_dialect: None,
        capabilities: CapabilitySet::of(&[
            Capability::Tree,
            Capability::Describe,
            Capability::BrowseRows,
            Capability::EditRows,
            Capability::RawFilter,
            Capability::KeysetPaging,
            Capability::EstimatedCount,
            Capability::ExactCount,
            Capability::Query,
            Capability::Cancel,
            Capability::StatementTimeout,
            Capability::Documents,
            Capability::Search,
            Capability::Console,
            Capability::ServerOverview,
        ]),
        objects_label: "indices".into(),
        connection: ConnectionForm {
            uses_host: true,
            uses_file: false,
            default_port: Some(DEFAULT_PORT),
            default_user: if elastic { "elastic".into() } else { "admin".into() },
            // A cluster has no databases; indices are the objects.
            database_label: String::new(),
            database_hint: String::new(),
            url: Some(UrlSpec {
                schemes: vec!["https".into(), "http".into()],
                example: if elastic {
                    "https://elastic@localhost:9200".into()
                } else {
                    "https://admin@localhost:9200".into()
                },
            }),
            auth,
            fields,
            ssh: true,
            // https. verifyFull = chain and host name, verifyCa = chain only, require = any certificate; `pin` is the
            // SHA-256 fingerprint Elasticsearch prints on first start.
            tls: Some(TlsSupport {
                modes: vec![TlsModeId::Disable, TlsModeId::Require, TlsModeId::VerifyCa, TlsModeId::VerifyFull],
                ca: true,
                client_cert: false,
                server_name: false,
                pin: true,
            }),
        },
        languages,
        filter: FilterSupport {
            structured: true,
            raw: Some(RawFilterLang::EsQueryDsl),
            placeholder: "Filter: Query DSL, e.g. { \"term\": { \"status\": \"paid\" } }".into(),
        },
        assistant: AssistantHints {
            test_data: "a _bulk request".into(),
            translate_to: Some(format!("an {} search request (Query DSL)", flavor.label())),
            ..AssistantHints::default()
        },
    }
}
