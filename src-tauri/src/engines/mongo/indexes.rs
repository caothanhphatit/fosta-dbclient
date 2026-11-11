//! The index operations of the collection workspace's Indexes section.
//!
//! - `mongo.indexes { database, collection, maxTimeMS? }` → `{ indexes, totalSize, stats: { sizes, usage, note },
//!   elapsedMs }` (reads). Each index is `{ name, key, keys: [{ field, kind }], options, size, usage: { ops, since } }`:
//!   `key` the key document, `keys` its fields in order (`kind` 1, -1 or the type: "text", "2dsphere", "2d",
//!   "hashed"), `options` the stored specification without `v`, `key`, `name` and `ns` (unique, sparse, hidden,
//!   partialFilterExpression, expireAfterSeconds, collation, weights, default_language, wildcardProjection …).
//!   Sizes come from `$collStats` storageStats (bytes), usage from `$indexStats` (operations since the server started
//!   or the index was created, summed over shards); either is null where the server or the user's privileges do not
//!   give it, and `stats.note` says why.
//! - `mongo.createIndex { database, collection, keys, name?, unique?, sparse?, hidden?, partialFilterExpression?,
//!   expireAfterSeconds?, collation?, wildcardProjection?, weights?, defaultLanguage? }` → `{ name, created, statement,
//!   elapsedMs }` (DDL). Without a name the server's default (`field_1_other_-1`) is used; `created` is false when an
//!   identical index existed. Modern servers build without blocking the collection, so there is no `background` flag.
//! - `mongo.hideIndex` / `mongo.unhideIndex { database, collection, name }` → `{ name, hidden, changed, statement }`
//!   (DDL, `collMod`): a hidden index is kept up to date but not used by the planner.
//! - `mongo.dropIndex { database, collection, name }` → `{ name, statement, elapsedMs }` (destructive); `_id_` and `*`
//!   are refused.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use mongodb::{
    bson::{doc, Bson, Document},
    Client, Collection,
};
use serde_json::{json, Value as Json};

use super::ops::{bool_arg, document_arg, elapsed_ms, name_json, relaxed, u64_arg};
use super::values;
use crate::error::{AppError, AppResult};

/// The `_id` index, which can be neither dropped nor hidden.
pub const ID_INDEX: &str = "_id_";
/// Key types besides 1 / -1.
pub const KEY_TYPES: [&str; 4] = ["text", "2dsphere", "2d", "hashed"];
/// Options of `mongo.createIndex` that are documents.
const DOCUMENT_OPTIONS: [&str; 4] = ["partialFilterExpression", "collation", "wildcardProjection", "weights"];
/// Specification keys that are not options.
const NOT_OPTIONS: [&str; 4] = ["v", "key", "name", "ns"];
/// The longest TTL the server accepts (a 32-bit signed int of seconds).
const MAX_TTL: u64 = i32::MAX as u64;

/// The `name` argument, trimmed and not empty.
pub fn index_name(args: &Json) -> AppResult<&str> {
    let name = args
        .get("name")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppError::new("name (the index name) is required"))?;
    if name.contains('\0') {
        return Err(AppError::new("An index name cannot hold a NUL character"));
    }
    Ok(name)
}

/// Whether the operation targets an index that must stay: `_id_`, or `*` (every index).
pub fn protected_index(args: &Json) -> bool {
    matches!(args.get("name").and_then(Json::as_str).map(str::trim), Some(ID_INDEX | "*"))
}

/// The key document of `mongo.createIndex`, checked: dotted field paths (a wildcard `$**` or `path.$**`), each 1, -1 or
/// one of [`KEY_TYPES`] (numbers normalised to 32-bit ints).
pub fn index_keys(args: &Json) -> AppResult<Document> {
    let keys = document_arg(args, "keys")?.ok_or_else(|| AppError::new("keys is required"))?;
    if keys.is_empty() {
        return Err(AppError::new("An index needs at least one key"));
    }
    let mut checked = Document::new();
    for (field, value) in keys {
        let wildcard = field == "$**" || field.ends_with(".$**");
        let path = if wildcard {
            field.strip_suffix("$**").unwrap_or_default().trim_end_matches('.')
        } else {
            &field
        };
        let bad_path = !path.is_empty() && path.split('.').any(|part| part.is_empty() || part.starts_with('$'));
        if field.trim().is_empty() || field.contains('\0') || bad_path || (!wildcard && path.is_empty()) {
            return Err(AppError::new(format!("'{field}' is not a field path")));
        }
        let direction = match &value {
            Bson::Int32(n) => Some(f64::from(*n)),
            Bson::Int64(n) => Some(*n as f64),
            Bson::Double(n) => Some(*n),
            _ => None,
        };
        let normalised = match (direction, &value) {
            (Some(n), _) if n == 1.0 || n == -1.0 => Bson::Int32(n as i32),
            (None, Bson::String(kind)) if KEY_TYPES.contains(&kind.as_str()) && !wildcard => Bson::String(kind.clone()),
            _ => {
                return Err(AppError::new(format!(
                    "The key {field} must be 1, -1{}",
                    if wildcard {
                        ""
                    } else {
                        ", \"text\", \"2dsphere\", \"2d\" or \"hashed\""
                    }
                )))
            }
        };
        checked.insert(field, normalised);
    }
    Ok(checked)
}

/// The server's default name of an index: `field_direction` joined by `_` (`a_1_b_-1`, `title_text`).
pub fn default_name(keys: &Document) -> String {
    keys.iter()
        .map(|(field, value)| match value {
            Bson::String(kind) => format!("{field}_{kind}"),
            Bson::Int32(n) => format!("{field}_{n}"),
            other => format!("{field}_{other}"),
        })
        .collect::<Vec<_>>()
        .join("_")
}

/// The options of `mongo.createIndex`, as the specification stores them.
pub fn index_options(args: &Json) -> AppResult<Document> {
    let mut options = Document::new();
    for flag in ["unique", "sparse", "hidden"] {
        if bool_arg(args, flag) {
            options.insert(flag, true);
        }
    }
    if let Some(seconds) = u64_arg(args, "expireAfterSeconds")? {
        if seconds > MAX_TTL {
            return Err(AppError::new(format!("expireAfterSeconds is at most {MAX_TTL}")));
        }
        options.insert("expireAfterSeconds", seconds as i32);
    }
    for key in DOCUMENT_OPTIONS {
        if let Some(document) = document_arg(args, key)?.filter(|document| !document.is_empty()) {
            options.insert(key, document);
        }
    }
    if let Some(language) = args
        .get("defaultLanguage")
        .and_then(Json::as_str)
        .map(str::trim)
        .filter(|language| !language.is_empty())
    {
        options.insert("default_language", language);
    }
    Ok(options)
}

/// The mongosh line that creates the index.
pub fn create_statement(collection: &str, keys: &Document, options: &Document) -> String {
    let mut statement = format!(
        "db.getCollection({}).createIndex({}",
        name_json(collection),
        values::document_text(keys)
    );
    if !options.is_empty() {
        statement.push_str(&format!(", {}", values::document_text(options)));
    }
    statement.push(')');
    statement
}

fn database_of(client: &Client, coll: &Collection<Document>) -> mongodb::Database {
    client.database(&coll.namespace().db)
}

/// One key of an index: the field and its direction or type.
fn key_entry(field: &str, value: &Bson) -> Json {
    let kind = match value {
        Bson::Int32(n) => json!(n.signum()),
        Bson::Int64(n) => json!(n.signum()),
        Bson::Double(n) if *n < 0.0 => json!(-1),
        Bson::Double(_) => json!(1),
        Bson::String(kind) => json!(kind),
        other => other.clone().into_relaxed_extjson(),
    };
    json!({ "field": field, "kind": kind })
}

/// The number in a BSON value (sizes and counters come as int32, int64 or double).
fn number(value: Option<&Bson>) -> Option<i64> {
    match value? {
        Bson::Int32(n) => Some(i64::from(*n)),
        Bson::Int64(n) => Some(*n),
        Bson::Double(n) => Some(*n as i64),
        _ => None,
    }
}

/// Index sizes in bytes by name (summed over shards) and the total, from `$collStats`.
async fn index_sizes(coll: &Collection<Document>, max_time: Duration) -> AppResult<(HashMap<String, i64>, i64)> {
    let pipeline = vec![doc! { "$collStats": { "storageStats": {} } }];
    let stats: Vec<Document> = coll.aggregate(pipeline).max_time(max_time).await?.try_collect().await?;
    let mut sizes = HashMap::new();
    let mut total = 0;
    for shard in stats {
        let Ok(storage) = shard.get_document("storageStats") else {
            continue;
        };
        total += number(storage.get("totalIndexSize")).unwrap_or(0);
        if let Ok(per_index) = storage.get_document("indexSizes") {
            for (name, size) in per_index {
                *sizes.entry(name.clone()).or_insert(0) += number(Some(size)).unwrap_or(0);
            }
        }
    }
    Ok((sizes, total))
}

/// Operations per index (summed over shards) and the earliest `since`, from `$indexStats`.
async fn index_usage(coll: &Collection<Document>, max_time: Duration) -> AppResult<HashMap<String, (i64, Option<String>)>> {
    let pipeline = vec![doc! { "$indexStats": {} }];
    let stats: Vec<Document> = coll.aggregate(pipeline).max_time(max_time).await?.try_collect().await?;
    let mut usage: HashMap<String, (i64, Option<String>)> = HashMap::new();
    for entry in stats {
        let Ok(name) = entry.get_str("name") else {
            continue;
        };
        let accesses = entry.get_document("accesses").ok();
        let ops = number(accesses.and_then(|accesses| accesses.get("ops"))).unwrap_or(0);
        let since = accesses
            .and_then(|accesses| accesses.get_datetime("since").ok())
            .and_then(|since| since.try_to_rfc3339_string().ok());
        let slot = usage.entry(name.to_owned()).or_insert((0, None));
        slot.0 += ops;
        if let Some(since) = since {
            // RFC 3339 texts in UTC compare in time order.
            if slot.1.as_ref().is_none_or(|known| since < *known) {
                slot.1 = Some(since);
            }
        }
    }
    Ok(usage)
}

/// `mongo.indexes`.
pub async fn indexes(client: &Client, coll: &Collection<Document>, max_time: Duration) -> AppResult<Json> {
    let started = Instant::now();
    let database = database_of(client, coll);
    let reply = database
        .run_command(doc! { "listIndexes": coll.name(), "cursor": { "batchSize": 1000 } })
        .await?;
    let specs: Vec<Document> = reply
        .get_document("cursor")
        .and_then(|cursor| cursor.get_array("firstBatch"))
        .map(|batch| batch.iter().filter_map(|spec| spec.as_document().cloned()).collect())
        .unwrap_or_default();
    let mut notes = Vec::new();
    let sizes = match index_sizes(coll, max_time).await {
        Ok(sizes) => Some(sizes),
        Err(error) => {
            notes.push(format!("Sizes are not available ({})", error.message()));
            None
        }
    };
    let usage = match index_usage(coll, max_time).await {
        Ok(usage) => Some(usage),
        Err(error) => {
            notes.push(format!("Usage is not available ({})", error.message()));
            None
        }
    };
    let indexes: Vec<Json> = specs
        .into_iter()
        .map(|spec| {
            let name = spec.get_str("name").unwrap_or_default().to_owned();
            let key = spec.get_document("key").cloned().unwrap_or_default();
            let keys: Vec<Json> = key.iter().map(|(field, value)| key_entry(field, value)).collect();
            let mut options = spec.clone();
            for skip in NOT_OPTIONS {
                options.remove(skip);
            }
            let size = sizes.as_ref().and_then(|(sizes, _)| sizes.get(&name).copied());
            let used = usage.as_ref().map(|usage| match usage.get(&name) {
                Some((ops, since)) => json!({ "ops": ops, "since": since }),
                None => json!({ "ops": 0, "since": null }),
            });
            json!({
                "name": name,
                "key": relaxed(key),
                "keys": keys,
                "options": relaxed(options),
                "size": size,
                "usage": used,
            })
        })
        .collect();
    Ok(json!({
        "indexes": indexes,
        "totalSize": sizes.as_ref().map(|(_, total)| *total),
        "stats": {
            "sizes": sizes.is_some(),
            "usage": usage.is_some(),
            "note": (!notes.is_empty()).then(|| notes.join("; ")),
        },
        "elapsedMs": elapsed_ms(started),
    }))
}

/// `mongo.createIndex`.
pub async fn create_index(client: &Client, coll: &Collection<Document>, args: &Json) -> AppResult<Json> {
    let keys = index_keys(args)?;
    let options = index_options(args)?;
    let name = match args.get("name").and_then(Json::as_str).map(str::trim) {
        Some(name) if !name.is_empty() => index_name(args)?.to_owned(),
        _ => default_name(&keys),
    };
    let mut shown = doc! { "name": &name };
    shown.extend(options.clone());
    let statement = create_statement(coll.name(), &keys, &shown);
    let mut spec = doc! { "key": keys, "name": &name };
    spec.extend(options);
    let started = Instant::now();
    let reply = database_of(client, coll)
        .run_command(doc! { "createIndexes": coll.name(), "indexes": [spec] })
        .await?;
    let before = number(reply.get("numIndexesBefore"));
    let after = number(reply.get("numIndexesAfter"));
    let created = match (before, after) {
        (Some(before), Some(after)) => after > before,
        _ => reply.get_str("note").is_err(),
    };
    Ok(json!({ "name": name, "created": created, "statement": statement, "elapsedMs": elapsed_ms(started) }))
}

/// `mongo.hideIndex` (`hidden` true) and `mongo.unhideIndex`.
pub async fn set_hidden(client: &Client, coll: &Collection<Document>, args: &Json, hidden: bool) -> AppResult<Json> {
    let name = index_name(args)?;
    if name == ID_INDEX {
        return Err(AppError::new("The _id index cannot be hidden"));
    }
    let reply = database_of(client, coll)
        .run_command(doc! { "collMod": coll.name(), "index": { "name": name, "hidden": hidden } })
        .await?;
    let old = reply.get_bool("hidden_old").ok();
    let new = reply.get_bool("hidden_new").ok();
    let changed = match (old, new) {
        (Some(old), Some(new)) => old != new,
        // The server leaves both out when nothing changed.
        _ => false,
    };
    let verb = if hidden { "hideIndex" } else { "unhideIndex" };
    Ok(json!({
        "name": name,
        "hidden": hidden,
        "changed": changed,
        "statement": format!("db.getCollection({}).{verb}({})", name_json(coll.name()), name_json(name)),
    }))
}

/// `mongo.dropIndex`.
pub async fn drop_index(client: &Client, coll: &Collection<Document>, args: &Json) -> AppResult<Json> {
    let name = index_name(args)?;
    if protected_index(args) {
        return Err(AppError::new(if name == ID_INDEX {
            "The _id index cannot be dropped"
        } else {
            "Drop one index at a time, by name"
        }));
    }
    let started = Instant::now();
    database_of(client, coll)
        .run_command(doc! { "dropIndexes": coll.name(), "index": name })
        .await?;
    Ok(json!({
        "name": name,
        "statement": format!("db.getCollection({}).dropIndex({})", name_json(coll.name()), name_json(name)),
        "elapsedMs": elapsed_ms(started),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_paths_with_a_direction_or_type() {
        let keys = index_keys(&json!({ "keys": { "a": 1, "b.c": -1.0, "d": { "$numberLong": "1" } } })).unwrap();
        assert_eq!(keys, doc! { "a": 1, "b.c": -1, "d": 1 });
        let keys = index_keys(&json!({ "keys": "{ \"title\": \"text\", \"loc\": \"2dsphere\" }" })).unwrap();
        assert_eq!(keys, doc! { "title": "text", "loc": "2dsphere" });
        assert_eq!(index_keys(&json!({ "keys": { "$**": 1 } })).unwrap(), doc! { "$**": 1 });
        assert_eq!(index_keys(&json!({ "keys": { "meta.$**": 1 } })).unwrap(), doc! { "meta.$**": 1 });
        for bad in [
            json!({}),
            json!({ "keys": {} }),
            json!({ "keys": { "a": 2 } }),
            json!({ "keys": { "a": "btree" } }),
            json!({ "keys": { "$a": 1 } }),
            json!({ "keys": { "a..b": 1 } }),
            json!({ "keys": { "a.$b": 1 } }),
            json!({ "keys": { "$**": "text" } }),
            json!({ "keys": [1] }),
        ] {
            assert!(index_keys(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn default_names_follow_the_server() {
        assert_eq!(default_name(&doc! { "a": 1, "b": -1 }), "a_1_b_-1");
        assert_eq!(default_name(&doc! { "title": "text" }), "title_text");
        assert_eq!(default_name(&doc! { "$**": 1 }), "$**_1");
    }

    #[test]
    fn options_and_statement() {
        let args = json!({
            "unique": true,
            "sparse": false,
            "expireAfterSeconds": 3600,
            "partialFilterExpression": "{ \"status\": \"active\" }",
            "collation": { "locale": "en", "strength": 2 },
            "weights": {},
            "defaultLanguage": " english ",
        });
        let options = index_options(&args).unwrap();
        assert_eq!(
            options,
            doc! {
                "unique": true,
                "expireAfterSeconds": 3600,
                "partialFilterExpression": { "status": "active" },
                "collation": { "locale": "en", "strength": 2 },
                "default_language": "english",
            }
        );
        assert!(index_options(&json!({ "expireAfterSeconds": -1 })).is_err());
        assert!(index_options(&json!({ "expireAfterSeconds": 3_000_000_000_u64 })).is_err());
        assert!(index_options(&json!({ "partialFilterExpression": [1] })).is_err());
        assert_eq!(
            create_statement("orders", &doc! { "a": 1 }, &doc! { "name": "a_1", "unique": true }),
            r#"db.getCollection("orders").createIndex({"a":1}, {"name":"a_1","unique":true})"#
        );
        assert_eq!(
            create_statement("orders", &doc! { "a": 1 }, &Document::new()),
            r#"db.getCollection("orders").createIndex({"a":1})"#
        );
    }

    #[test]
    fn names_and_protected_indexes() {
        assert_eq!(index_name(&json!({ "name": " a_1 " })).unwrap(), "a_1");
        assert!(index_name(&json!({ "name": "" })).is_err());
        assert!(index_name(&json!({})).is_err());
        assert!(protected_index(&json!({ "name": "_id_" })));
        assert!(protected_index(&json!({ "name": "*" })));
        assert!(!protected_index(&json!({ "name": "a_1" })));
    }

    #[test]
    fn key_entries_give_direction_or_type() {
        assert_eq!(key_entry("a", &Bson::Int32(-1)), json!({ "field": "a", "kind": -1 }));
        assert_eq!(key_entry("a", &Bson::Double(1.0)), json!({ "field": "a", "kind": 1 }));
        assert_eq!(
            key_entry("t", &Bson::String("text".into())),
            json!({ "field": "t", "kind": "text" })
        );
    }
}
