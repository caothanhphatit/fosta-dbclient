//! Contract suites for MongoDB against `dev/compose/mongo.yml`. The fixture writes the same documents as
//! `dev/seed/mongo/contract.js` (database fosta_contract) through database commands before each suite.
//! Override the server with FOSTA_CONTRACT_MONGO_HOST / _PORT / _USER / _PASSWORD.

use serde_json::json;

use crate::contract::{testing::suites::*, Category, ConnectSecrets, ConnectionSpec, Filter, NodeKind, ObjectRef, RawFilterLang};

fn env(name: &str, default: &str) -> String {
    std::env::var(format!("FOSTA_CONTRACT_MONGO_{name}")).unwrap_or_else(|_| default.to_owned())
}

/// Database commands that reset the collections.
fn seed() -> Vec<String> {
    let items: Vec<serde_json::Value> = (1..=1000)
        .map(|i: i64| {
            json!({
                "_id": i,
                "name": format!("item {i:04}"),
                "price": i as f64 * 1.25,
                "active": i % 2 == 0,
                "born": format!("2020-01-{:02}", 1 + i % 28),
                "meta": { "n": i, "tags": ["a", "b"] },
                "note": if i % 3 == 0 { serde_json::Value::Null } else { json!(format!("note {i}")) },
            })
        })
        .collect();
    let children: Vec<serde_json::Value> = (1..=200)
        .map(|i: i64| json!({ "_id": i, "item_id": i, "label": format!("child {i}") }))
        .collect();
    vec![
        json!({ "delete": "contract_items", "deletes": [{ "q": {}, "limit": 0 }] }).to_string(),
        json!({ "delete": "contract_children", "deletes": [{ "q": {}, "limit": 0 }] }).to_string(),
        json!({ "insert": "contract_items", "documents": items }).to_string(),
        json!({ "insert": "contract_children", "documents": children }).to_string(),
        json!({ "createIndexes": "contract_items", "indexes": [{ "key": { "name": 1 }, "name": "uq_contract_items_name", "unique": true }] })
            .to_string(),
    ]
}

fn key_filter(key: i64) -> Filter {
    Filter::Raw {
        language: RawFilterLang::MongoJson,
        text: json!({ "_id": key }).to_string(),
    }
}

pub fn fixture() -> Fixture {
    Fixture {
        engine: "mongodb",
        spec: ConnectionSpec {
            engine: "mongodb".into(),
            host: env("HOST", "127.0.0.1"),
            port: env("PORT", "27017").parse().unwrap_or(27017),
            user: env("USER", "root"),
            database: Some("admin".into()),
            ..ConnectionSpec::default()
        },
        secrets: ConnectSecrets::password(&env("PASSWORD", "studio-root")),
        table: ObjectRef::new(Some("fosta_contract"), None, "contract_items", NodeKind::COLLECTION),
        child: ObjectRef::new(Some("fosta_contract"), None, "contract_children", NodeKind::COLLECTION),
        key_column: "_id",
        name_column: "name",
        seed: seed(),
        seed_database: Some("fosta_contract"),
        key_filter,
        sleep_statement: None,
        syntax_error: r#"{ "find": "#,
        constraint_violation: r#"{ "insert": "contract_items", "documents": [{ "_id": 1, "name": "dup" }] }"#,
        multi_statement: r#"{ "find": "contract_items", "limit": 3 }"#,
        insert_probe: r#"{ "insert": "contract_items", "documents": [{ "_id": 5000, "name": "probe" }] }"#,
        count_probe: r#"{ "count": "contract_items", "query": { "_id": 5000 } }"#,
        classify_samples: &[
            (r#"{ "find": "t" }"#, Category::Read),
            (r#"{ "insert": "t", "documents": [] }"#, Category::Write),
            (r#"{ "drop": "t" }"#, Category::Destructive),
        ],
        language: "mongo-command",
        gaps: &[
            Gap::StructuredFilter,
            Gap::Concurrency,
            Gap::Keyset,
            Gap::TypedValues,
            Gap::ErrorMapping,
        ],
    }
}

crate::contract_tests!(contract_mongo, fixture());
