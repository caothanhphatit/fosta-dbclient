//! The schema validation operations of the collection workspace's Validation section.
//!
//! - `mongo.validation { database, collection }` → `{ validator, validationLevel, validationAction, type, statement,
//!   elapsedMs }` (reads): the collection's options from `listCollections`. `validator` is canonical Extended JSON, null
//!   when the collection has none; `validationLevel` / `validationAction` are the server's defaults (`strict`, `error`)
//!   when not set; `type` is `collection`, `view` or `timeseries` (null when the collection does not exist).
//! - `mongo.setValidation { database, collection, validator?, validationLevel?, validationAction? }` → `{ validator,
//!   validationLevel, validationAction, statement, elapsedMs }` (DDL, `collMod`). At least one of the three is given;
//!   an empty validator (`{}`) removes the rules. `validationLevel` is `off`, `strict` or `moderate`,
//!   `validationAction` `error` or `warn`.
//! - `mongo.validationCheck { database, collection, validator, limit?, maxTimeMS? }` → `{ failing, capped, total,
//!   samples, statement, elapsedMs }` (reads): how many existing documents the validator would refuse, counted with
//!   `{ $nor: [validator] }` (which reads `$jsonSchema` as well as query operators) up to [`MAX_COUNT`] (`capped` when
//!   the count stopped there), the collection's estimated size, and the first `limit` failing documents (default
//!   [`DEFAULT_SAMPLES`], at most [`MAX_SAMPLES`]) as relaxed Extended JSON. One time bound covers both reads.
//!
//! Server-side JavaScript in a validator (`$where`, `$function` inside `$expr`, `$accumulator`) is refused by `invoke`
//! and the classifier: `validator` is one of the query-document arguments they check.

use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use mongodb::{
    bson::{doc, Bson, Document},
    Client, Collection,
};
use serde_json::{json, Value as Json};

use super::ops::{document_arg, elapsed_ms, name_json, relaxed, u64_arg};
use super::values;
use crate::error::{AppError, AppResult};

/// The accepted `validationLevel` and `validationAction` values.
pub const LEVELS: [&str; 3] = ["off", "strict", "moderate"];
pub const ACTIONS: [&str; 2] = ["error", "warn"];
/// Failing documents counted at most, and sampled by default and at most.
pub const MAX_COUNT: u64 = 100_000;
pub const DEFAULT_SAMPLES: u64 = 5;
pub const MAX_SAMPLES: u64 = 20;

fn choice<'a>(args: &'a Json, key: &str, allowed: &[&str]) -> AppResult<Option<&'a str>> {
    match args.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::String(value)) if value.trim().is_empty() => Ok(None),
        Some(Json::String(value)) if allowed.contains(&value.trim()) => Ok(Some(value.trim())),
        Some(value) => Err(AppError::new(format!("{key} must be {}, not {value}", allowed.join(", ")))),
    }
}

/// The `collMod` command of `mongo.setValidation`, checked: a validator document (empty removes the rules), a level and
/// an action, at least one of them.
pub fn coll_mod(collection: &str, args: &Json) -> AppResult<Document> {
    let mut command = doc! { "collMod": collection };
    if let Some(validator) = document_arg(args, "validator")? {
        command.insert("validator", validator);
    }
    if let Some(level) = choice(args, "validationLevel", &LEVELS)? {
        command.insert("validationLevel", level);
    }
    if let Some(action) = choice(args, "validationAction", &ACTIONS)? {
        command.insert("validationAction", action);
    }
    if command.len() == 1 {
        return Err(AppError::new("Give a validator, a validationLevel or a validationAction"));
    }
    Ok(command)
}

/// The mongosh line of a `collMod` command.
pub fn coll_mod_statement(command: &Document) -> String {
    format!("db.runCommand({})", values::document_text(command))
}

/// The filter matching the documents a validator refuses: `{ $nor: [validator] }` (nothing for an empty validator).
pub fn failing_filter(validator: &Document) -> Option<Document> {
    (!validator.is_empty()).then(|| doc! { "$nor": [validator.clone()] })
}

fn database_of(client: &Client, coll: &Collection<Document>) -> mongodb::Database {
    client.database(&coll.namespace().db)
}

/// The collection's specification from `listCollections`, None when there is no such collection.
async fn collection_spec(client: &Client, coll: &Collection<Document>) -> AppResult<Option<Document>> {
    let reply = database_of(client, coll)
        .run_command(doc! { "listCollections": 1, "filter": { "name": coll.name() } })
        .await?;
    Ok(reply
        .get_document("cursor")
        .and_then(|cursor| cursor.get_array("firstBatch"))
        .ok()
        .and_then(|batch| batch.iter().find_map(|spec| spec.as_document().cloned())))
}

/// `mongo.validation`.
pub async fn validation(client: &Client, coll: &Collection<Document>) -> AppResult<Json> {
    let started = Instant::now();
    let spec = collection_spec(client, coll).await?;
    let options = spec
        .as_ref()
        .and_then(|spec| spec.get_document("options").ok())
        .cloned()
        .unwrap_or_default();
    let validator = options
        .get_document("validator")
        .ok()
        .filter(|validator| !validator.is_empty())
        .cloned();
    let level = options.get_str("validationLevel").unwrap_or("strict");
    let action = options.get_str("validationAction").unwrap_or("error");
    let kind = spec.as_ref().and_then(|spec| spec.get_str("type").ok());
    Ok(json!({
        "validator": validator.map(|validator| Bson::Document(validator).into_canonical_extjson()),
        "validationLevel": level,
        "validationAction": action,
        "type": kind,
        "statement": format!("db.getCollectionInfos({{ name: {} }})[0].options", name_json(coll.name())),
        "elapsedMs": elapsed_ms(started),
    }))
}

/// `mongo.setValidation`.
pub async fn set_validation(client: &Client, coll: &Collection<Document>, args: &Json) -> AppResult<Json> {
    let command = coll_mod(coll.name(), args)?;
    let statement = coll_mod_statement(&command);
    let started = Instant::now();
    database_of(client, coll).run_command(command.clone()).await?;
    Ok(json!({
        "validator": command.get_document("validator").ok().map(|validator| Bson::Document(validator.clone()).into_canonical_extjson()),
        "validationLevel": command.get_str("validationLevel").ok(),
        "validationAction": command.get_str("validationAction").ok(),
        "statement": statement,
        "elapsedMs": elapsed_ms(started),
    }))
}

/// `mongo.validationCheck`.
pub async fn validation_check(coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let validator = document_arg(args, "validator")?.ok_or_else(|| AppError::new("validator is required"))?;
    let limit = u64_arg(args, "limit")?.unwrap_or(DEFAULT_SAMPLES).clamp(1, MAX_SAMPLES);
    let started = Instant::now();
    let total = coll.estimated_document_count().max_time(max_time).await.ok();
    let Some(filter) = failing_filter(&validator) else {
        return Ok(json!({
            "failing": 0,
            "capped": false,
            "total": total,
            "samples": [],
            "statement": "",
            "elapsedMs": elapsed_ms(started),
        }));
    };
    let statement = format!(
        "db.getCollection({}).find({})",
        name_json(coll.name()),
        values::document_text(&filter)
    );
    let failing = coll.count_documents(filter.clone()).limit(MAX_COUNT).max_time(max_time).await?;
    let samples: Vec<Document> = coll
        .find(filter)
        .sort(doc! { "_id": 1 })
        .limit(limit as i64)
        .max_time(max_time)
        .await?
        .try_collect()
        .await?;
    Ok(json!({
        "failing": failing,
        "capped": failing >= MAX_COUNT,
        "total": total,
        "samples": samples.into_iter().map(relaxed).collect::<Vec<_>>(),
        "statement": statement,
        "elapsedMs": elapsed_ms(started),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coll_mod_takes_validator_level_and_action() {
        let command = coll_mod(
            "orders",
            &json!({
                "validator": "{ \"$jsonSchema\": { \"required\": [\"status\"] } }",
                "validationLevel": "moderate",
                "validationAction": " warn ",
            }),
        )
        .unwrap();
        assert_eq!(
            command,
            doc! {
                "collMod": "orders",
                "validator": { "$jsonSchema": { "required": ["status"] } },
                "validationLevel": "moderate",
                "validationAction": "warn",
            }
        );
        assert_eq!(
            coll_mod_statement(&command),
            r#"db.runCommand({"collMod":"orders","validator":{"$jsonSchema":{"required":["status"]}},"validationLevel":"moderate","validationAction":"warn"})"#
        );
        // An empty validator removes the rules.
        assert_eq!(
            coll_mod("c", &json!({ "validator": {} })).unwrap(),
            doc! { "collMod": "c", "validator": {} }
        );
        assert_eq!(
            coll_mod("c", &json!({ "validationLevel": "off" })).unwrap(),
            doc! { "collMod": "c", "validationLevel": "off" }
        );
        for bad in [
            json!({}),
            json!({ "validationLevel": "lax" }),
            json!({ "validationAction": "ignore" }),
            json!({ "validationLevel": 1 }),
            json!({ "validator": [1] }),
            json!({ "validator": "{ broken" }),
        ] {
            assert!(coll_mod("c", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn failing_documents_are_the_negated_validator() {
        assert_eq!(failing_filter(&Document::new()), None);
        assert_eq!(
            failing_filter(&doc! { "$jsonSchema": { "required": ["a"] } }),
            Some(doc! { "$nor": [{ "$jsonSchema": { "required": ["a"] } }] })
        );
    }
}
