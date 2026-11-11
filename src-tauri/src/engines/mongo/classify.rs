//! MongoDB database commands written as JSON (the query tab's input), classified by the command name: the first key
//! of the object. Aggregations with `$out` / `$merge` write; deletes and multi-updates with an empty filter are
//! destructive. The engine operations (`mongo.find` …) are classified by name: they read, refuse server-side JavaScript
//! and refuse names they do not know.

use super::ops::{denied_operator, OPERATIONS};
use crate::{
    contract::classify::{Category, Classification, StatementInfo},
    engines::common::sqlclassify::excerpt,
};

/// MongoDB database command names by category; anything else is [`Category::Unknown`].
const MONGO_READ: [&str; 27] = [
    "find",
    "aggregate",
    "count",
    "distinct",
    "listCollections",
    "listIndexes",
    "listDatabases",
    "dbStats",
    "collStats",
    "dataSize",
    "explain",
    "getMore",
    "killCursors",
    "buildInfo",
    "hello",
    "isMaster",
    "ismaster",
    "ping",
    "serverStatus",
    "connectionStatus",
    "validate",
    "getParameter",
    "listCommands",
    "replSetGetStatus",
    "top",
    "whatsmyuri",
    "rolesInfo",
];
/// Commands that only read but expose users, credentials, other sessions' operations (with their literal values), server
/// logs or the host and command line: administration, so neither the assistant nor a Read-only workspace runs them.
const MONGO_SENSITIVE_READ: [&str; 5] = ["usersInfo", "getLog", "currentOp", "hostInfo", "getCmdLineOpts"];
/// Collections holding users, roles and keys (credential hashes); reading them is administration.
const SYSTEM_SECRET_COLLECTIONS: [&str; 3] = ["system.users", "system.roles", "system.keys"];
/// Aggregation stages that read server or session internals.
const SENSITIVE_STAGES: [&str; 3] = ["$currentOp", "$listSessions", "$listLocalSessions"];
const SENSITIVE_READ_REASON: &str =
    "Shows users, credentials, other sessions' operations, server logs or the host; treated as administration";
const MONGO_WRITE: [&str; 5] = ["insert", "update", "delete", "findAndModify", "bulkWrite"];
const MONGO_DESTRUCTIVE: [&str; 5] = ["drop", "dropDatabase", "renameCollection", "dropIndexes", "convertToCapped"];
const MONGO_DDL: [&str; 5] = ["create", "createIndexes", "collMod", "dropSearchIndex", "createSearchIndexes"];
const MONGO_TRANSACTION: [&str; 3] = ["startSession", "commitTransaction", "abortTransaction"];

/// Classifies a MongoDB database command written as JSON (the query tab's input).
pub fn classify_mongo(text: &str) -> Classification {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return Classification::single(StatementInfo {
                category: Category::Unknown,
                kind: "unparsed".to_owned(),
                tables: Vec::new(),
                excerpt: excerpt(text),
                reason: Some(format!("Not a JSON command ({error})")),
                refused: None,
            })
        }
    };
    let Some((name, target)) = value.as_object().and_then(|object| object.iter().next()) else {
        return Classification::single(StatementInfo {
            category: Category::Unknown,
            kind: "unparsed".to_owned(),
            tables: Vec::new(),
            excerpt: excerpt(text),
            reason: Some("A MongoDB command is a JSON object whose first key is the command name".to_owned()),
            refused: None,
        });
    };
    let tables = target.as_str().map(|collection| vec![collection.to_owned()]).unwrap_or_default();
    let (category, reason) = mongo_category(name, &value);
    Classification::single(StatementInfo {
        category,
        kind: name.clone(),
        tables,
        excerpt: excerpt(text),
        reason,
        refused: None,
    })
}

/// Classifies an engine operation (`mongo.find`, `mongo.count`, `mongo.distinct`, `mongo.resolveRef`): every one reads,
/// unless it targets credentials (administration); server-side JavaScript in any query document and unknown names are
/// refused.
pub fn classify_operation(name: &str, args: &serde_json::Value) -> StatementInfo {
    if !OPERATIONS.contains(&name) {
        return StatementInfo::refused(name, name, "unknown operation");
    }
    let excerpt = format!("{name} {args}");
    if let Some(operator) = denied_operator(args) {
        return StatementInfo::refused(name, &excerpt, &format!("{operator} runs JavaScript on the server"));
    }
    let tables = args
        .get("collection")
        .and_then(|collection| collection.as_str())
        .map(|collection| vec![collection.to_owned()])
        .unwrap_or_default();
    let info = if touches_secrets(args) {
        StatementInfo::new(Category::Admin, name, &excerpt).with_reason(SENSITIVE_READ_REASON)
    } else {
        StatementInfo::new(Category::Read, name, &excerpt)
    };
    info.with_tables(tables)
}

/// A flag such as `showPrivileges` that is set: anything but absent, false, 0 or null.
fn flag_set(command: &serde_json::Value, key: &str) -> bool {
    match command.get(key) {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(on)) => *on,
        Some(serde_json::Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(_) => true,
    }
}

/// Whether any string in the command names a credential collection (a `find` target, a `$lookup` / `$unionWith` source)
/// or any key is a stage that reads other sessions.
fn touches_secrets(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => SYSTEM_SECRET_COLLECTIONS
            .iter()
            .any(|collection| text == collection || text.ends_with(&format!(".{collection}"))),
        serde_json::Value::Array(items) => items.iter().any(touches_secrets),
        serde_json::Value::Object(object) => object
            .iter()
            .any(|(key, item)| SENSITIVE_STAGES.contains(&key.as_str()) || touches_secrets(item)),
        _ => false,
    }
}

fn mongo_category(name: &str, command: &serde_json::Value) -> (Category, Option<String>) {
    let shows_privileges =
        matches!(name, "rolesInfo" | "connectionStatus") && (flag_set(command, "showPrivileges") || flag_set(command, "showCredentials"));
    if MONGO_SENSITIVE_READ.contains(&name) || shows_privileges || touches_secrets(command) {
        return (Category::Admin, Some(SENSITIVE_READ_REASON.to_owned()));
    }
    if name == "aggregate" {
        let writes = command
            .get("pipeline")
            .and_then(|pipeline| pipeline.as_array())
            .is_some_and(|stages| {
                stages
                    .iter()
                    .any(|stage| stage.get("$out").is_some() || stage.get("$merge").is_some())
            });
        return if writes {
            (Category::Write, Some("$out / $merge write the result into a collection".to_owned()))
        } else {
            (Category::Read, None)
        };
    }
    if name == "delete" && mongo_matches_everything(command, "deletes") {
        return (Category::Destructive, Some("An empty filter deletes every document".to_owned()));
    }
    if name == "update" && mongo_updates_everything(command) {
        return (
            Category::Destructive,
            Some("An empty filter with multi updates every document".to_owned()),
        );
    }
    if name == "explain" {
        // Explain never applies writes, whatever it explains.
        return (Category::Read, None);
    }
    if MONGO_READ.contains(&name) {
        (Category::Read, None)
    } else if MONGO_WRITE.contains(&name) {
        (Category::Write, None)
    } else if MONGO_DESTRUCTIVE.contains(&name) {
        (Category::Destructive, Some(format!("{name} removes data")))
    } else if MONGO_DDL.contains(&name) {
        (Category::Ddl, None)
    } else if MONGO_TRANSACTION.contains(&name) {
        (Category::Transaction, None)
    } else if name.contains("User") || name.contains("Role") || name.starts_with("replSet") || ADMIN_MONGO.contains(&name) {
        (Category::Admin, None)
    } else {
        (
            Category::Unknown,
            Some(format!("{name} is not a known command, so it is treated like a write")),
        )
    }
}

const ADMIN_MONGO: [&str; 8] = [
    "killOp",
    "shutdown",
    "setParameter",
    "fsync",
    "fsyncUnlock",
    "logRotate",
    "compact",
    "setFeatureCompatibilityVersion",
];

fn mongo_matches_everything(command: &serde_json::Value, list: &str) -> bool {
    command.get(list).and_then(|items| items.as_array()).is_some_and(|items| {
        items
            .iter()
            .any(|item| item.get("q").and_then(|q| q.as_object()).is_some_and(|q| q.is_empty()))
    })
}

fn mongo_updates_everything(command: &serde_json::Value) -> bool {
    command.get("updates").and_then(|items| items.as_array()).is_some_and(|items| {
        items.iter().any(|item| {
            let empty = item.get("q").and_then(|q| q.as_object()).is_some_and(|q| q.is_empty());
            let multi = item.get("multi").and_then(|multi| multi.as_bool()).unwrap_or(false);
            empty && multi
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mongo_commands_by_name() {
        let category = |text: &str| classify_mongo(text).statements[0].category;
        assert_eq!(category(r#"{ "find": "products", "filter": { "stock": 0 } }"#), Category::Read);
        assert_eq!(
            category(r#"{ "aggregate": "c", "pipeline": [ { "$match": {} } ], "cursor": {} }"#),
            Category::Read
        );
        assert_eq!(
            category(r#"{ "aggregate": "c", "pipeline": [ { "$out": "copy" } ], "cursor": {} }"#),
            Category::Write
        );
        assert_eq!(
            category(r#"{ "aggregate": "c", "pipeline": [ { "$merge": { "into": "x" } } ], "cursor": {} }"#),
            Category::Write
        );
        assert_eq!(category(r#"{ "count": "c" }"#), Category::Read);
        assert_eq!(category(r#"{ "listCollections": 1 }"#), Category::Read);
        assert_eq!(category(r#"{ "insert": "c", "documents": [ { "a": 1 } ] }"#), Category::Write);
        assert_eq!(
            category(r#"{ "update": "c", "updates": [ { "q": { "a": 1 }, "u": { "$set": { "b": 2 } } } ] }"#),
            Category::Write
        );
        assert_eq!(
            category(r#"{ "update": "c", "updates": [ { "q": {}, "u": { "$set": { "b": 2 } }, "multi": true } ] }"#),
            Category::Destructive
        );
        assert_eq!(
            category(r#"{ "delete": "c", "deletes": [ { "q": { "a": 1 }, "limit": 1 } ] }"#),
            Category::Write
        );
        assert_eq!(
            category(r#"{ "delete": "c", "deletes": [ { "q": {}, "limit": 0 } ] }"#),
            Category::Destructive
        );
        assert_eq!(
            category(r#"{ "findAndModify": "c", "query": {}, "remove": true }"#),
            Category::Write
        );
        assert_eq!(category(r#"{ "drop": "c" }"#), Category::Destructive);
        assert_eq!(category(r#"{ "dropDatabase": 1 }"#), Category::Destructive);
        assert_eq!(category(r#"{ "renameCollection": "a.b", "to": "a.c" }"#), Category::Destructive);
        assert_eq!(category(r#"{ "createIndexes": "c", "indexes": [] }"#), Category::Ddl);
        assert_eq!(category(r#"{ "collMod": "c" }"#), Category::Ddl);
        assert_eq!(category(r#"{ "createUser": "x", "pwd": "y", "roles": [] }"#), Category::Admin);
        assert_eq!(category(r#"{ "frobnicate": 1 }"#), Category::Unknown);
        assert_eq!(category("not json"), Category::Unknown);
        assert_eq!(classify_mongo(r#"{ "drop": "events" }"#).statements[0].tables, ["events"]);
    }

    #[test]
    fn reads_that_expose_secrets_are_admin() {
        let category = |text: &str| classify_mongo(text).statements[0].category;
        for admin in [
            r#"{ "usersInfo": 1 }"#,
            r#"{ "usersInfo": { "user": "app", "db": "admin" }, "showCredentials": true }"#,
            r#"{ "rolesInfo": 1, "showPrivileges": true }"#,
            r#"{ "rolesInfo": "readWrite", "showPrivileges": 1 }"#,
            r#"{ "connectionStatus": 1, "showPrivileges": true }"#,
            r#"{ "getLog": "global" }"#,
            r#"{ "currentOp": 1 }"#,
            r#"{ "hostInfo": 1 }"#,
            r#"{ "getCmdLineOpts": 1 }"#,
            r#"{ "aggregate": 1, "pipeline": [ { "$currentOp": {} } ], "cursor": {} }"#,
            r#"{ "find": "system.users" }"#,
            r#"{ "count": "system.keys" }"#,
            r#"{ "aggregate": "orders", "pipeline": [ { "$lookup": { "from": "system.users", "as": "u", "pipeline": [] } } ], "cursor": {} }"#,
        ] {
            assert_eq!(category(admin), Category::Admin, "{admin}");
        }
        for read in [
            r#"{ "rolesInfo": 1 }"#,
            r#"{ "rolesInfo": 1, "showPrivileges": false }"#,
            r#"{ "connectionStatus": 1 }"#,
            r#"{ "serverStatus": 1 }"#,
            r#"{ "find": "users" }"#,
        ] {
            assert_eq!(category(read), Category::Read, "{read}");
        }
    }

    #[test]
    fn engine_operations_read_and_refuse_server_javascript() {
        use serde_json::json;
        let base = json!({ "database": "shop", "collection": "orders" });
        let with = |extra: serde_json::Value| {
            let mut args = base.clone();
            args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            args
        };
        for (name, args) in [
            (
                "mongo.find",
                with(json!({ "filter": { "status": "paid" }, "sort": { "_id": -1 }, "limit": 20 })),
            ),
            (
                "mongo.count",
                with(json!({ "filter": "{ \"status\": \"paid\" }", "estimated": false })),
            ),
            ("mongo.distinct", with(json!({ "field": "status" }))),
            ("mongo.resolveRef", with(json!({ "id": { "$oid": "65f1a2b3c4d5e6f708192a3b" } }))),
        ] {
            let info = classify_operation(name, &args);
            assert_eq!(info.category, Category::Read, "{name}");
            assert_eq!(info.refused, None, "{name}");
            assert_eq!(info.tables, ["orders"], "{name}");
        }
        let unknown = classify_operation("mongo.dropEverything", &base);
        assert_eq!(unknown.refused.as_deref(), Some("unknown operation"));
        for (name, args) in [
            ("mongo.find", with(json!({ "filter": { "$where": "sleep(100) || true" } }))),
            ("mongo.find", with(json!({ "filter": "{ \"$or\": [ { \"$where\": \"true\" } ] }" }))),
            (
                "mongo.find",
                with(json!({ "projection": { "x": { "$function": { "body": "f", "args": [], "lang": "js" } } } })),
            ),
            (
                "mongo.count",
                with(json!({ "filter": { "$expr": { "$function": { "body": "f", "args": [], "lang": "js" } } } })),
            ),
            ("mongo.distinct", with(json!({ "field": "a", "filter": { "$where": "true" } }))),
        ] {
            let info = classify_operation(name, &args);
            assert!(
                info.refused.as_deref().is_some_and(|why| why.contains("JavaScript")),
                "{name}: {args}"
            );
        }
        let secret = classify_operation("mongo.find", &json!({ "database": "admin", "collection": "system.users" }));
        assert_eq!(secret.category, Category::Admin);
        assert_eq!(secret.refused, None);
    }
}
