//! MongoDB database commands written as JSON (the query tab's input), classified by the command name: the first key
//! of the object. Aggregations with `$merge` write and with `$out` (which drops and replaces its target) are
//! destructive; deletes and multi-updates with a filter that matches everything are destructive, and so is a TTL index
//! (`expireAfterSeconds`, which deletes documents on its own). The engine operations (`mongo.find` …) are classified by
//! name: reads, the bulk update (a write, destructive with a match-all filter), the bulk delete (destructive) and the
//! aggregations (a read; with `$merge` a write, with `$out` destructive; the preview refuses both, and every
//! aggregation refuses change streams), the index listing (a read), creating, hiding and unhiding an index (DDL; a TTL
//! index destructive) and dropping one (destructive; never `_id_`); they refuse server-side JavaScript, database or
//! collection names with surrounding whitespace, and names they do not know.

use super::{
    aggregate::{pipeline_arg, read_targets, stream_stage, write_stage, write_target},
    bulk::{filter_matches_everything, matches_everything},
    indexes::protected_index,
    ops::{denied_operator, namespace_args, OPERATIONS},
};
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
/// Collections holding users, roles and keys (credential hashes, also as left by mongorestore and auth upgrades),
/// other sessions' queries with their literal values (`system.profile`), every write to every database (`oplog.rs`)
/// and the server's command line (`startup_log`); reading them is administration, in any database.
const SYSTEM_SECRET_COLLECTIONS: [&str; 8] = [
    "system.users",
    "system.roles",
    "system.keys",
    "system.backup_users",
    "system.new_users",
    "system.profile",
    "oplog.rs",
    "startup_log",
];
/// Databases every collection of which is administration: `local` holds the oplog, the replica set's configuration
/// and the startup log.
const SECRET_DATABASES: [&str; 1] = ["local"];
/// Aggregation stages that read server or session internals, or other sessions' queries with their literal values.
const SENSITIVE_STAGES: [&str; 6] = [
    "$currentOp",
    "$listSessions",
    "$listLocalSessions",
    "$planCacheStats",
    "$listSampledQueries",
    "$queryStats",
];
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

/// Classifies an engine operation by name: `mongo.find`, `mongo.count`, `mongo.distinct`, `mongo.resolveRef`, `mongo.schema` and
/// `mongo.updatePreview` read; `mongo.updateMany` writes (destructive with a match-all filter); `mongo.deleteMany` is always
/// destructive; `mongo.indexes` reads, `mongo.createIndex` / `mongo.hideIndex` / `mongo.unhideIndex` are DDL (a TTL index is
/// destructive) and `mongo.dropIndex` is destructive (the `_id_` index, or `*`, is refused); `mongo.validation` and
/// `mongo.validationCheck` read, `mongo.setValidation` is DDL (`collMod`); `mongo.explain` reads (refused with `$out` /
/// `$merge` or a change stream). Any of them is administration when it targets credentials, other sessions' queries or the
/// `local` database; server-side JavaScript in any query document, update, partial filter or validator, padded or missing
/// database and collection names, and unknown names are refused.
pub fn classify_operation(name: &str, args: &serde_json::Value) -> StatementInfo {
    if !OPERATIONS.contains(&name) {
        return StatementInfo::refused(name, name, "unknown operation");
    }
    let excerpt = format!("{name} {args}");
    // The same check (and the same untrimmed names) as the call itself, so both see one namespace.
    let database = match namespace_args(args) {
        Ok((database, _)) => database,
        Err(error) => return StatementInfo::refused(name, &excerpt, error.message()),
    };
    if let Some(operator) = denied_operator(args) {
        return StatementInfo::refused(name, &excerpt, &format!("{operator} runs JavaScript on the server"));
    }
    let tables = args
        .get("collection")
        .and_then(|collection| collection.as_str())
        .map(|collection| vec![collection.to_owned()])
        .unwrap_or_default();
    let secret_database = SECRET_DATABASES.contains(&database);
    if matches!(name, "mongo.aggregatePreview" | "mongo.aggregate" | "mongo.explain") {
        return classify_aggregation(name, args, &excerpt, tables, secret_database);
    }
    if matches!(name, "mongo.dropIndex" | "mongo.hideIndex" | "mongo.unhideIndex") && protected_index(args) {
        return StatementInfo::refused(name, &excerpt, "the _id index (or every index at once) cannot be dropped or hidden");
    }
    let info = if secret_database || touches_secrets(args) {
        StatementInfo::new(Category::Admin, name, &excerpt).with_reason(SENSITIVE_READ_REASON)
    } else {
        match name {
            "mongo.updateMany" if filter_matches_everything(args) => StatementInfo::new(Category::Destructive, name, &excerpt)
                .with_reason("The filter matches every document, so this updates the whole collection"),
            "mongo.updateMany" => StatementInfo::new(Category::Write, name, &excerpt),
            "mongo.deleteMany" if filter_matches_everything(args) => StatementInfo::new(Category::Destructive, name, &excerpt)
                .with_reason("The filter matches every document, so this deletes the whole collection"),
            "mongo.deleteMany" => StatementInfo::new(Category::Destructive, name, &excerpt).with_reason("Deletes every matching document"),
            "mongo.createIndex" if is_ttl(args) => StatementInfo::new(Category::Destructive, name, &excerpt)
                .with_reason("A TTL index (expireAfterSeconds) deletes documents on its own once their date has passed"),
            "mongo.createIndex" => StatementInfo::new(Category::Ddl, name, &excerpt).with_reason("Builds a new index on the collection"),
            "mongo.hideIndex" => {
                StatementInfo::new(Category::Ddl, name, &excerpt).with_reason("Hides the index from the query planner (collMod)")
            }
            "mongo.unhideIndex" => {
                StatementInfo::new(Category::Ddl, name, &excerpt).with_reason("Lets the query planner use the index again (collMod)")
            }
            "mongo.dropIndex" => StatementInfo::new(Category::Destructive, name, &excerpt)
                .with_reason("Drops the index; queries that used it scan instead until it is built again"),
            "mongo.setValidation" => StatementInfo::new(Category::Ddl, name, &excerpt)
                .with_reason("Changes the collection's validation rules (collMod); later writes are checked against them"),
            // `mongo.updatePreview` simulates the update in an aggregation: it reads.
            _ => StatementInfo::new(Category::Read, name, &excerpt),
        }
    };
    info.with_tables(tables)
}

/// An aggregation: the collections it reads (`$lookup`, `$graphLookup`, `$unionWith`) and writes are its tables, and
/// reading credentials through any of them is administration. `mongo.aggregatePreview` only reads, so `$out` / `$merge`
/// are refused there; `mongo.aggregate` with `$merge` writes and with `$out` (which drops and replaces the target
/// collection, possibly the source itself) is destructive. Change streams never end and are refused everywhere. A
/// pipeline that does not parse is refused (it could not run either).
fn classify_aggregation(
    name: &str,
    args: &serde_json::Value,
    excerpt: &str,
    mut tables: Vec<String>,
    secret_database: bool,
) -> StatementInfo {
    let stages = match pipeline_arg(args) {
        Ok(stages) => stages.unwrap_or_default(),
        Err(error) => return StatementInfo::refused(name, excerpt, error.message()),
    };
    if let Some(stage) = stream_stage(&stages) {
        return StatementInfo::refused(name, excerpt, &format!("{stage} opens a change stream, which never ends"));
    }
    let writes = write_stage(&stages);
    if let (Some(stage), "mongo.aggregatePreview") = (writes, name) {
        return StatementInfo::refused(name, excerpt, &format!("{stage} writes into a collection; a preview only reads"));
    }
    if let (Some(stage), "mongo.explain") = (writes, name) {
        return StatementInfo::refused(name, excerpt, &format!("{stage} writes into a collection; explain only reads"));
    }
    for table in read_targets(&stages).into_iter().chain(write_target(&stages)) {
        if !tables.contains(&table) {
            tables.push(table);
        }
    }
    let info = if secret_database || touches_secrets(args) || touches_secrets(&serde_json::Value::Array(stages)) {
        StatementInfo::new(Category::Admin, name, excerpt).with_reason(SENSITIVE_READ_REASON)
    } else {
        match writes {
            Some("$out") => StatementInfo::new(Category::Destructive, name, excerpt)
                .with_reason("$out drops the target collection (which can be the source itself) and replaces it with the result"),
            Some(_) => StatementInfo::new(Category::Write, name, excerpt).with_reason("$merge writes the result into a collection"),
            None => StatementInfo::new(Category::Read, name, excerpt),
        }
    };
    info.with_tables(tables)
}

/// Index options (or a `collMod` index change) that make a TTL index: any `expireAfterSeconds`, 0 included.
fn is_ttl(options: &serde_json::Value) -> bool {
    options.get("expireAfterSeconds").is_some_and(|seconds| !seconds.is_null())
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

/// Whether any string in the command names a sensitive collection (a `find` target, a `$lookup` / `$unionWith` source),
/// any `{ db, coll }` names the `local` database, or any key is a stage that reads other sessions.
fn touches_secrets(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(text) => SYSTEM_SECRET_COLLECTIONS
            .iter()
            .any(|collection| text == collection || text.ends_with(&format!(".{collection}"))),
        serde_json::Value::Array(items) => items.iter().any(touches_secrets),
        serde_json::Value::Object(object) => {
            object
                .get("db")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|database| SECRET_DATABASES.contains(&database))
                || object
                    .iter()
                    .any(|(key, item)| SENSITIVE_STAGES.contains(&key.as_str()) || touches_secrets(item))
        }
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
        let stages = command
            .get("pipeline")
            .and_then(|pipeline| pipeline.as_array())
            .cloned()
            .unwrap_or_default();
        return match write_stage(&stages) {
            Some("$out") => (
                Category::Destructive,
                Some("$out drops the target collection (which can be the source itself) and replaces it with the result".to_owned()),
            ),
            Some(_) => (Category::Write, Some("$merge writes the result into a collection".to_owned())),
            None => (Category::Read, None),
        };
    }
    if name == "createIndexes"
        && command
            .get("indexes")
            .and_then(|indexes| indexes.as_array())
            .is_some_and(|indexes| indexes.iter().any(is_ttl))
    {
        return (
            Category::Destructive,
            Some("A TTL index (expireAfterSeconds) deletes documents on its own once their date has passed".to_owned()),
        );
    }
    if name == "collMod" && command.get("index").is_some_and(is_ttl) {
        return (
            Category::Destructive,
            Some("Changing a TTL index's expireAfterSeconds can delete documents on its own".to_owned()),
        );
    }
    if name == "delete" && mongo_matches_everything(command, "deletes") {
        return (
            Category::Destructive,
            Some("The filter matches every document, so this deletes them all".to_owned()),
        );
    }
    if name == "update" && mongo_updates_everything(command) {
        return (
            Category::Destructive,
            Some("The filter matches every document and multi is set, so this updates them all".to_owned()),
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
    command
        .get(list)
        .and_then(|items| items.as_array())
        .is_some_and(|items| items.iter().any(|item| item.get("q").is_some_and(matches_everything)))
}

fn mongo_updates_everything(command: &serde_json::Value) -> bool {
    command.get("updates").and_then(|items| items.as_array()).is_some_and(|items| {
        items.iter().any(|item| {
            let empty = item.get("q").is_some_and(matches_everything);
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
            Category::Destructive
        );
        assert_eq!(
            category(r#"{ "createIndexes": "c", "indexes": [ { "key": { "at": 1 }, "name": "at_1", "expireAfterSeconds": 0 } ] }"#),
            Category::Destructive
        );
        assert_eq!(
            category(r#"{ "collMod": "c", "index": { "name": "at_1", "expireAfterSeconds": 60 } }"#),
            Category::Destructive
        );
        assert_eq!(
            category(r#"{ "delete": "c", "deletes": [ { "q": { "$expr": true }, "limit": 0 } ] }"#),
            Category::Destructive
        );
        assert_eq!(
            category(
                r#"{ "update": "c", "updates": [ { "q": { "_id": { "$exists": true } }, "u": { "$set": { "b": 2 } }, "multi": true } ] }"#
            ),
            Category::Destructive
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
            r#"{ "find": "system.backup_users" }"#,
            r#"{ "find": "system.new_users" }"#,
            r#"{ "find": "system.profile" }"#,
            r#"{ "find": "oplog.rs" }"#,
            r#"{ "find": "startup_log" }"#,
            r#"{ "aggregate": 1, "pipeline": [ { "$queryStats": {} } ], "cursor": {} }"#,
            r#"{ "aggregate": "c", "pipeline": [ { "$planCacheStats": {} } ], "cursor": {} }"#,
            r#"{ "aggregate": 1, "pipeline": [ { "$listSampledQueries": {} } ], "cursor": {} }"#,
            r#"{ "aggregate": "c", "pipeline": [ { "$unionWith": { "db": "local", "coll": "x" } } ], "cursor": {} }"#,
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
            (
                "mongo.schema",
                with(json!({ "sampleMode": "filtered", "filter": "{ \"status\": \"paid\" }", "sampleSize": 500 })),
            ),
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
            (
                "mongo.schema",
                with(json!({ "sampleMode": "filtered", "filter": "{ \"$where\": \"true\" }" })),
            ),
        ] {
            let info = classify_operation(name, &args);
            assert!(
                info.refused.as_deref().is_some_and(|why| why.contains("JavaScript")),
                "{name}: {args}"
            );
        }
        let schema_of_users = classify_operation("mongo.schema", &json!({ "database": "admin", "collection": "system.users" }));
        assert_eq!(schema_of_users.category, Category::Admin);
        let secret = classify_operation("mongo.find", &json!({ "database": "admin", "collection": "system.users" }));
        assert_eq!(secret.category, Category::Admin);
        assert_eq!(secret.refused, None);
        for (database, collection) in [
            ("admin", "system.backup_users"),
            ("admin", "system.new_users"),
            ("shop", "system.profile"),
            ("local", "startup_log"),
            ("local", "oplog.rs"),
            ("local", "system.replset"),
            ("local", "anything"),
        ] {
            let info = classify_operation("mongo.find", &json!({ "database": database, "collection": collection }));
            assert_eq!(info.category, Category::Admin, "{database}.{collection}");
        }
        // Names are classified exactly as the call uses them: padded, empty or missing names are refused, not trimmed.
        for args in [
            json!({ "database": "admin", "collection": " system.users" }),
            json!({ "database": "admin ", "collection": "system.users" }),
            json!({ "database": "shop", "collection": "" }),
            json!({ "collection": "orders" }),
            json!({ "database": "shop", "collection": "a\u{0}" }),
        ] {
            let info = classify_operation("mongo.find", &args);
            assert!(info.refused.is_some(), "{args}");
        }
    }

    #[test]
    fn aggregations_read_write_or_are_refused() {
        use serde_json::json;
        let args = |pipeline: serde_json::Value| json!({ "database": "shop", "collection": "orders", "pipeline": pipeline });
        let preview = classify_operation(
            "mongo.aggregatePreview",
            &args(json!([{ "$match": { "status": "paid" } }, "{ \"$group\": { \"_id\": \"$customer\", \"n\": { \"$sum\": 1 } } }"])),
        );
        assert_eq!((preview.category, preview.refused.as_deref()), (Category::Read, None));
        assert_eq!(preview.tables, ["orders"]);
        let read = classify_operation("mongo.aggregate", &args(json!([{ "$sort": { "total": -1 } }])));
        assert_eq!(read.category, Category::Read);
        // A preview never writes.
        for pipeline in [json!([{ "$out": "copy" }]), json!("[{ \"$merge\": { \"into\": \"t\" } }]")] {
            let info = classify_operation("mongo.aggregatePreview", &args(pipeline.clone()));
            assert!(info.refused.as_deref().is_some_and(|why| why.contains("only reads")), "{pipeline}");
        }
        let merge = classify_operation(
            "mongo.aggregate",
            &args(json!([{ "$match": {} }, { "$merge": { "into": "totals" } }])),
        );
        assert_eq!((merge.category, merge.refused.as_deref()), (Category::Write, None));
        assert_eq!(merge.tables, ["orders", "totals"]);
        let out = classify_operation("mongo.aggregate", &args(json!(["{ \"$out\": \"copy\" }"])));
        assert_eq!(out.category, Category::Destructive);
        // Emptying the source through $out is as destructive as a drop.
        let emptied = classify_operation(
            "mongo.aggregate",
            &args(json!([{ "$match": { "_id": null } }, { "$out": "orders" }])),
        );
        assert_eq!(emptied.category, Category::Destructive);
        // Change streams never end: refused for the preview, the run and explain.
        for name in ["mongo.aggregatePreview", "mongo.aggregate"] {
            for pipeline in [
                json!([{ "$changeStream": {} }]),
                json!(["{ \"$changeStreamSplitLargeEvent\": {} }"]),
            ] {
                let info = classify_operation(name, &args(pipeline.clone()));
                assert!(
                    info.refused.as_deref().is_some_and(|why| why.contains("change stream")),
                    "{name}: {pipeline}"
                );
            }
        }
        let explained = classify_operation(
            "mongo.explain",
            &json!({ "database": "shop", "collection": "orders", "kind": "aggregate", "pipeline": [{ "$changeStream": {} }] }),
        );
        assert!(explained.refused.as_deref().is_some_and(|why| why.contains("change stream")));
        // Anything in the local database is administration.
        let oplog = classify_operation(
            "mongo.aggregate",
            &json!({ "database": "local", "collection": "anything", "pipeline": [] }),
        );
        assert_eq!(oplog.category, Category::Admin);
        let queries = classify_operation("mongo.aggregatePreview", &args(json!([{ "$queryStats": {} }])));
        assert_eq!(queries.category, Category::Admin);
        assert_eq!(out.tables, ["orders", "copy"]);
        // Other collections read are tables; credentials read through them are administration.
        let lookup = classify_operation(
            "mongo.aggregatePreview",
            &args(
                json!([{ "$lookup": { "from": "customers", "localField": "c", "foreignField": "_id", "as": "c" } }, { "$unionWith": "archive" }]),
            ),
        );
        assert_eq!(lookup.category, Category::Read);
        assert_eq!(lookup.tables, ["orders", "customers", "archive"]);
        for pipeline in [
            json!([{ "$lookup": { "from": "system.users", "localField": "u", "foreignField": "user", "as": "u" } }]),
            json!(["{ \"$unionWith\": { \"coll\": \"system.users\" } }"]),
            json!([{ "$currentOp": {} }]),
        ] {
            let info = classify_operation(
                "mongo.aggregatePreview",
                &json!({ "database": "admin", "collection": "x", "pipeline": pipeline }),
            );
            assert_eq!((info.category, info.refused.as_deref()), (Category::Admin, None), "{pipeline}");
        }
        // Server-side JavaScript anywhere in the pipeline, as values or text, is refused.
        for pipeline in [
            json!([{ "$match": { "$where": "true" } }]),
            json!([{ "$project": { "x": { "$function": { "body": "f", "args": [], "lang": "js" } } } }]),
            json!(["{ \"$group\": { \"_id\": null, \"x\": { \"$accumulator\": {} } } }"]),
            json!("[{ \"$match\": { \"$where\": \"true\" } }]"),
        ] {
            for name in ["mongo.aggregatePreview", "mongo.aggregate"] {
                let info = classify_operation(name, &args(pipeline.clone()));
                assert!(
                    info.refused.as_deref().is_some_and(|why| why.contains("JavaScript")),
                    "{name}: {pipeline}"
                );
            }
        }
        // A pipeline that does not parse is refused.
        let broken = classify_operation("mongo.aggregate", &args(json!(["{ nope"])));
        assert!(broken.refused.is_some());
    }

    #[test]
    fn explains_read_and_refuse_writes_and_server_javascript() {
        use serde_json::json;
        let args = |extra: serde_json::Value| {
            let mut args = json!({ "database": "shop", "collection": "orders" });
            args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            args
        };
        for extra in [
            json!({ "kind": "find", "filter": { "status": "paid" }, "verbosity": "executionStats" }),
            json!({ "kind": "find", "filter": "{ \"total\": { \"$gt\": 1 } }", "sort": { "total": -1 }, "verbosity": "allPlansExecution" }),
            json!({ "kind": "aggregate", "pipeline": [{ "$match": {} }, "{ \"$group\": { \"_id\": \"$c\" } }"] }),
        ] {
            let info = classify_operation("mongo.explain", &args(extra.clone()));
            assert_eq!((info.category, info.refused.as_deref()), (Category::Read, None), "{extra}");
            assert_eq!(info.tables, ["orders"]);
        }
        let lookup = classify_operation(
            "mongo.explain",
            &args(
                json!({ "kind": "aggregate", "pipeline": [{ "$lookup": { "from": "customers", "localField": "c", "foreignField": "_id", "as": "c" } }] }),
            ),
        );
        assert_eq!(lookup.tables, ["orders", "customers"]);
        for pipeline in [json!([{ "$out": "copy" }]), json!("[{ \"$merge\": { \"into\": \"t\" } }]")] {
            let info = classify_operation("mongo.explain", &args(json!({ "kind": "aggregate", "pipeline": pipeline })));
            assert!(
                info.refused.as_deref().is_some_and(|why| why.contains("explain only reads")),
                "{info:?}"
            );
        }
        for extra in [
            json!({ "filter": { "$where": "true" } }),
            json!({ "filter": "{ \"$expr\": { \"$function\": { \"body\": \"f\", \"args\": [], \"lang\": \"js\" } } }" }),
            json!({ "kind": "aggregate", "pipeline": [{ "$group": { "_id": null, "x": { "$accumulator": {} } } }] }),
        ] {
            let info = classify_operation("mongo.explain", &args(extra.clone()));
            assert!(info.refused.as_deref().is_some_and(|why| why.contains("JavaScript")), "{extra}");
        }
        assert!(
            classify_operation("mongo.explain", &args(json!({ "kind": "aggregate", "pipeline": ["{ nope"] })))
                .refused
                .is_some()
        );
        let secret = classify_operation(
            "mongo.explain",
            &json!({ "database": "admin", "collection": "system.users", "kind": "find" }),
        );
        assert_eq!(secret.category, Category::Admin);
    }

    #[test]
    fn index_operations_are_read_ddl_or_destructive() {
        use serde_json::json;
        let args = |extra: serde_json::Value| {
            let mut args = json!({ "database": "shop", "collection": "orders" });
            args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            args
        };
        let info = |name: &str, extra: serde_json::Value| classify_operation(name, &args(extra));
        let listed = info("mongo.indexes", json!({}));
        assert_eq!((listed.category, listed.refused.as_deref()), (Category::Read, None));
        assert_eq!(listed.tables, ["orders"]);
        let create = info(
            "mongo.createIndex",
            json!({ "keys": { "status": 1, "createdAt": -1 }, "unique": true, "partialFilterExpression": { "status": "paid" } }),
        );
        assert_eq!((create.category, create.refused.as_deref()), (Category::Ddl, None));
        assert_eq!(create.tables, ["orders"]);
        // A TTL index deletes documents by itself.
        for seconds in [json!(0), json!(3600)] {
            let ttl = info(
                "mongo.createIndex",
                json!({ "keys": { "createdAt": 1 }, "expireAfterSeconds": seconds }),
            );
            assert_eq!((ttl.category, ttl.refused.as_deref()), (Category::Destructive, None), "{seconds}");
        }
        let plain = info(
            "mongo.createIndex",
            json!({ "keys": { "createdAt": 1 }, "expireAfterSeconds": null }),
        );
        assert_eq!(plain.category, Category::Ddl);
        for name in ["mongo.hideIndex", "mongo.unhideIndex"] {
            assert_eq!(info(name, json!({ "name": "status_1" })).category, Category::Ddl, "{name}");
        }
        let drop = info("mongo.dropIndex", json!({ "name": "status_1" }));
        assert_eq!((drop.category, drop.refused.as_deref()), (Category::Destructive, None));
        assert_eq!(drop.tables, ["orders"]);
        // The _id index (and every index at once) stays.
        for name in ["mongo.dropIndex", "mongo.hideIndex", "mongo.unhideIndex"] {
            for index in ["_id_", " _id_ ", "*"] {
                assert!(info(name, json!({ "name": index })).refused.is_some(), "{name} {index}");
            }
        }
        // Server-side JavaScript in a partial filter, as JSON or text, is refused.
        for partial in [json!({ "$where": "true" }), json!("{ \"$expr\": { \"$function\": {} } }")] {
            let refused = info(
                "mongo.createIndex",
                json!({ "keys": { "a": 1 }, "partialFilterExpression": partial }),
            );
            assert!(
                refused.refused.as_deref().is_some_and(|why| why.contains("JavaScript")),
                "{refused:?}"
            );
        }
        let secret = classify_operation("mongo.indexes", &json!({ "database": "admin", "collection": "system.users" }));
        assert_eq!(secret.category, Category::Admin);
        let secret = classify_operation(
            "mongo.createIndex",
            &json!({ "database": "admin", "collection": "system.users", "keys": { "user": 1 } }),
        );
        assert_eq!(secret.category, Category::Admin);
    }

    #[test]
    fn validation_operations_read_or_are_ddl() {
        use serde_json::json;
        let args = |extra: serde_json::Value| {
            let mut args = json!({ "database": "shop", "collection": "orders" });
            args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            args
        };
        let schema = json!({ "$jsonSchema": { "bsonType": "object", "required": ["status"] } });
        for (name, extra) in [
            ("mongo.validation", json!({})),
            ("mongo.validationCheck", json!({ "validator": schema.clone(), "limit": 5 })),
        ] {
            let info = classify_operation(name, &args(extra));
            assert_eq!((info.category, info.refused.as_deref()), (Category::Read, None), "{name}");
            assert_eq!(info.tables, ["orders"]);
        }
        for extra in [
            json!({ "validator": schema.clone(), "validationLevel": "strict", "validationAction": "error" }),
            json!({ "validationLevel": "off" }),
            json!({ "validator": {} }),
        ] {
            let info = classify_operation("mongo.setValidation", &args(extra.clone()));
            assert_eq!((info.category, info.refused.as_deref()), (Category::Ddl, None), "{extra}");
        }
        // Server-side JavaScript in a validator, as JSON or text, is refused for the check and for applying it.
        for validator in [
            json!({ "$where": "true" }),
            json!({ "$expr": { "$function": { "body": "f", "args": [], "lang": "js" } } }),
            json!("{ \"$and\": [ { \"$expr\": { \"$function\": {} } } ] }"),
        ] {
            for name in ["mongo.validationCheck", "mongo.setValidation"] {
                let refused = classify_operation(name, &args(json!({ "validator": validator.clone() })));
                assert!(
                    refused.refused.as_deref().is_some_and(|why| why.contains("JavaScript")),
                    "{name}: {refused:?}"
                );
            }
        }
        let secret = classify_operation(
            "mongo.setValidation",
            &json!({ "database": "admin", "collection": "system.users", "validator": {} }),
        );
        assert_eq!(secret.category, Category::Admin);
    }

    #[test]
    fn bulk_operations_are_classified_per_operation_and_filter() {
        use serde_json::json;
        let args = |filter: serde_json::Value| json!({ "database": "shop", "collection": "orders", "filter": filter, "update": { "$set": { "status": "archived" } } });
        let category = |name: &str, args: serde_json::Value| {
            let info = classify_operation(name, &args);
            assert_eq!(info.refused, None, "{name} {args}");
            assert_eq!(info.tables, ["orders"], "{name}");
            info.category
        };
        let some = json!({ "status": "paid" });
        assert_eq!(category("mongo.updatePreview", args(some.clone())), Category::Read);
        assert_eq!(category("mongo.updatePreview", args(json!({}))), Category::Read);
        assert_eq!(category("mongo.updateMany", args(some.clone())), Category::Write);
        assert_eq!(
            category("mongo.updateMany", args(json!("{ \"status\": \"paid\" }"))),
            Category::Write
        );
        for empty in [json!({}), json!(""), json!("{}"), json!(null), json!("{ broken")] {
            assert_eq!(category("mongo.updateMany", args(empty.clone())), Category::Destructive, "{empty}");
            assert_eq!(category("mongo.deleteMany", args(empty.clone())), Category::Destructive, "{empty}");
        }
        assert_eq!(category("mongo.deleteMany", args(some)), Category::Destructive);
        // Filters that trivially match everything are as destructive as an empty one.
        for all in [
            json!({ "$expr": true }),
            json!({ "_id": { "$exists": true } }),
            json!({ "$and": [{}] }),
            json!("{ \"$or\": [ {}, { \"a\": 1 } ] }"),
        ] {
            let info = classify_operation("mongo.updateMany", &args(all.clone()));
            assert_eq!(info.category, Category::Destructive, "{all}");
            assert!(info.reason.as_deref().is_some_and(|why| why.contains("every document")), "{all}");
        }
        let no_filter = json!({ "database": "shop", "collection": "orders", "update": { "$inc": { "n": 1 } } });
        assert_eq!(category("mongo.updateMany", no_filter), Category::Destructive);
        // Server-side JavaScript anywhere, including the update (a pipeline with $function), is refused.
        for (name, args) in [
            (
                "mongo.updateMany",
                json!({ "database": "s", "collection": "c", "filter": { "$where": "true" }, "update": { "$set": { "a": 1 } } }),
            ),
            (
                "mongo.updateMany",
                json!({ "database": "s", "collection": "c", "filter": { "a": 1 }, "update": [{ "$set": { "a": { "$function": { "body": "f", "args": [], "lang": "js" } } } }] }),
            ),
            (
                "mongo.updatePreview",
                json!({ "database": "s", "collection": "c", "update": "[{ \"$set\": { \"a\": { \"$accumulator\": {} } } }]" }),
            ),
            (
                "mongo.deleteMany",
                json!({ "database": "s", "collection": "c", "filter": "{ \"$where\": \"true\" }" }),
            ),
        ] {
            let info = classify_operation(name, &args);
            assert!(
                info.refused.as_deref().is_some_and(|why| why.contains("JavaScript")),
                "{name}: {args}"
            );
        }
        // Every operation the module runs is classified (none falls through as unknown).
        for name in OPERATIONS {
            assert_eq!(classify_operation(name, &args(json!({ "a": 1 }))).refused, None, "{name}");
        }
        assert_eq!(
            classify_operation("mongo.deleteMany", &json!({ "database": "admin", "collection": "system.users" })).category,
            Category::Admin
        );
    }
}
