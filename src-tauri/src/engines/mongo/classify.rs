//! MongoDB database commands written as JSON (the query tab's input), classified by the command name: the first key
//! of the object. Aggregations with `$out` / `$merge` write; deletes and multi-updates with an empty filter are
//! destructive.

use crate::{
    contract::classify::{Category, Classification, StatementInfo},
    engines::common::sqlclassify::excerpt,
};

/// MongoDB database command names by category; anything else is [`Category::Unknown`].
const MONGO_READ: [&str; 31] = [
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
    "hostInfo",
    "validate",
    "getParameter",
    "currentOp",
    "listCommands",
    "replSetGetStatus",
    "getLog",
    "top",
    "whatsmyuri",
    "usersInfo",
    "rolesInfo",
];
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

fn mongo_category(name: &str, command: &serde_json::Value) -> (Category, Option<String>) {
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
}
