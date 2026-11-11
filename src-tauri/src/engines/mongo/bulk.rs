//! Bulk changes from the collection workspace's query bar: preview an update on the documents a filter matches, then
//! update or delete all of them.
//!
//! - `mongo.updatePreview { database, collection, filter?, update, sample?, maxTimeMS? }` →
//!   `{ matched, sample: [{ before, after, changed }], elapsedMs, statement }`. Reads only: the matching documents are
//!   counted, the first `sample` of them (by `_id`, default 5, at most 20) are read, and the update is simulated on
//!   those with an aggregation (`$match` on their `_id`s, then stages that do what the update operators do). Operators
//!   without an exact aggregation equivalent are refused instead of guessed: see [`simulate`].
//! - `mongo.updateMany { database, collection, filter?, update }` → `{ matched, modified, elapsedMs }`: update operators
//!   or an aggregation pipeline (no replacement documents, no upsert).
//! - `mongo.deleteMany { database, collection, filter? }` → `{ deleted, elapsedMs }`.
//!
//! The classifier makes `updateMany` a write (destructive with a filter that matches everything, see
//! [`matches_everything`]) and `deleteMany` always destructive;
//! server-side JavaScript is refused in every argument. The writes have no time bound (the driver has none for them).

use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use mongodb::{
    bson::{doc, Bson, Document},
    Collection,
};
use serde_json::{json, Value as Json};

use super::{
    ops::{document_arg, json_arg},
    values::{self, ID_FIELD},
};
use crate::error::{AppError, AppResult};

/// Documents a preview shows when no size is given, and the most it shows.
const DEFAULT_SAMPLE: u64 = 5;
const MAX_SAMPLE: u64 = 20;

/// Stages an update pipeline may have (the server allows these only).
const PIPELINE_STAGES: [&str; 6] = ["$set", "$addFields", "$project", "$unset", "$replaceRoot", "$replaceWith"];

/// An update: update operators (`{ $set: … }`) or an aggregation pipeline.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Operators(Document),
    Pipeline(Vec<Document>),
}

impl Update {
    fn text(&self) -> String {
        match self {
            Update::Operators(document) => values::document_text(document),
            Update::Pipeline(stages) => {
                let stages: Vec<String> = stages.iter().map(values::document_text).collect();
                format!("[{}]", stages.join(", "))
            }
        }
    }
}

/// The `update` argument (a JSON value or JSON text).
pub fn update_arg(args: &Json) -> AppResult<Update> {
    let value = json_arg(args, "update")?.ok_or_else(|| AppError::new("update is required, e.g. { $set: { status: 'archived' } }"))?;
    let bson = Bson::try_from(value).map_err(|error| AppError::new(format!("update is not valid Extended JSON ({error})")))?;
    match bson {
        Bson::Document(document) => {
            if document.is_empty() {
                return Err(AppError::new(
                    "The update is empty; write update operators, e.g. { $set: { status: 'archived' } }",
                ));
            }
            if let Some(key) = document.keys().find(|key| !key.starts_with('$')) {
                return Err(AppError::new(format!(
                    "The update must use update operators such as $set (it has the field {key}); replacing whole documents is not a bulk update"
                )));
            }
            Ok(Update::Operators(document))
        }
        Bson::Array(stages) => {
            if stages.is_empty() {
                return Err(AppError::new("The update pipeline is empty"));
            }
            let stages = stages
                .into_iter()
                .map(|stage| match stage {
                    Bson::Document(stage)
                        if stage.len() == 1 && PIPELINE_STAGES.contains(&stage.keys().next().map(String::as_str).unwrap_or_default()) =>
                    {
                        Ok(stage)
                    }
                    _ => Err(AppError::new(format!(
                        "An update pipeline has only these stages, one per element: {}",
                        PIPELINE_STAGES.join(", ")
                    ))),
                })
                .collect::<AppResult<Vec<_>>>()?;
            Ok(Update::Pipeline(stages))
        }
        _ => Err(AppError::new("update must be a document of update operators or a pipeline")),
    }
}

/// Whether the filter argument matches every document: absent, empty text, `{}` or a trivially-true form
/// ([`matches_everything`]). A filter that does not parse counts as matching everything here, so it is classified the
/// stricter way (running it fails anyway).
pub fn filter_matches_everything(args: &Json) -> bool {
    match json_arg(args, "filter") {
        Ok(None) => true,
        Ok(Some(filter)) => matches_everything(&filter),
        Err(_) => true,
    }
}

/// Whether a filter document trivially matches every document: `{}`, `{ $expr: true }` (or any constant truthy
/// expression, `$literal` included), `{ _id: { $exists: true } }`, `{ $jsonSchema: {} }`, `{ $comment: … }` alone,
/// `{ $and: [ … ] }` of such filters or `{ $or: [ … ] }` with one of them. This catches pasted or injected match-all
/// filters; it is a guard against mistakes, not a proof (an expression such as `{ $eq: [1, 1] }` is not evaluated).
pub fn matches_everything(filter: &Json) -> bool {
    let Some(object) = filter.as_object() else {
        return false;
    };
    object.iter().all(|(key, value)| match key.as_str() {
        "$comment" => true,
        "$and" => value.as_array().is_some_and(|items| items.iter().all(matches_everything)),
        "$or" => value.as_array().is_some_and(|items| items.iter().any(matches_everything)),
        "$expr" => constant_truthy(value),
        "$jsonSchema" => value.as_object().is_some_and(|schema| schema.is_empty()),
        ID_FIELD => value
            .as_object()
            .is_some_and(|condition| !condition.is_empty() && condition.iter().all(|(op, flag)| op == "$exists" && literal_truthy(flag))),
        _ => false,
    })
}

/// An aggregation expression that is a constant and true: `true`, a non-zero number, a text that is not a field path
/// (`$…`), an array, or `{ $literal: <truthy> }`.
fn constant_truthy(value: &Json) -> bool {
    match value {
        Json::String(text) => !text.starts_with('$'),
        Json::Object(object) if object.len() == 1 => object.get("$literal").is_some_and(literal_truthy),
        Json::Array(_) => true,
        other => literal_truthy(other),
    }
}

/// A value MongoDB treats as true: anything but `false`, `0`, `null` (and missing).
fn literal_truthy(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(on) => *on,
        Json::Number(number) => number.as_f64() != Some(0.0),
        _ => true,
    }
}

fn refuse(operator: &str, why: &str) -> AppError {
    AppError::new(format!(
        "{operator} cannot be previewed: {why}. Nothing was guessed; run it without a preview only if you are sure."
    ))
}

/// A path an update names, checked for the preview: dotted field names, no array positions (`tags.0`, `$`, `$[]`),
/// which an aggregation addresses differently, and never `_id`.
fn preview_path(operator: &str, path: &str) -> AppResult<String> {
    if path == ID_FIELD || path.starts_with("_id.") {
        return Err(AppError::new("_id cannot be changed by an update"));
    }
    if path.is_empty() || path.split('.').any(|part| part.is_empty() || part.starts_with('$')) {
        return Err(refuse(operator, &format!("'{path}' is not a plain field path")));
    }
    if path.split('.').any(|part| part.bytes().all(|byte| byte.is_ascii_digit())) {
        return Err(refuse(operator, &format!("'{path}' names an array position")));
    }
    Ok(path.to_owned())
}

fn is_number(value: &Bson) -> bool {
    matches!(value, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::Decimal128(_))
}

fn missing(path: &str) -> Document {
    doc! { "$eq": [{ "$type": format!("${path}") }, "missing"] }
}

/// The aggregation stages that do what `update` does to a document, or an error for what has no exact equivalent:
/// `$set`, `$unset`, `$inc`, `$mul`, `$min`, `$max`, `$rename`, `$currentDate` (dates) and `$push` (a value or
/// `$each` alone) are simulated; `$addToSet`, `$pop`, `$pull`, `$pullAll`, `$bit`, `$setOnInsert`, `$push` modifiers,
/// timestamps and array positions are refused. Pipelines are their own simulation.
pub fn simulate(update: &Update) -> AppResult<Vec<Document>> {
    let operators = match update {
        Update::Pipeline(stages) => return Ok(stages.clone()),
        Update::Operators(operators) => operators,
    };
    let mut stages = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let mut claim = |path: String| -> AppResult<String> {
        if let Some(other) = paths.iter().find(|other| super::tabular::overlaps(other, &path)) {
            return Err(AppError::new(format!("Updating the path '{path}' would conflict with '{other}'")));
        }
        paths.push(path.clone());
        Ok(path)
    };
    for (operator, fields) in operators {
        let Bson::Document(fields) = fields else {
            return Err(AppError::new(format!("{operator} takes a document of fields")));
        };
        match operator.as_str() {
            "$set" | "$inc" | "$mul" | "$min" | "$max" | "$currentDate" | "$push" => {
                let mut set = Document::new();
                for (path, value) in fields {
                    let path = claim(preview_path(operator, path)?)?;
                    let field = format!("${path}");
                    let literal = || doc! { "$literal": value.clone() };
                    let expression: Bson = match operator.as_str() {
                        "$set" => literal().into(),
                        "$inc" | "$mul" if !is_number(value) => return Err(AppError::new(format!("{operator} needs a number for {path}"))),
                        "$inc" => doc! { "$add": [{ "$ifNull": [&field, 0] }, value.clone()] }.into(),
                        "$mul" => doc! { "$multiply": [{ "$ifNull": [&field, 0] }, value.clone()] }.into(),
                        // Update `$min` / `$max` compare in BSON order (null included) and set a missing field.
                        "$min" | "$max" => {
                            let compare = if operator == "$min" { "$lt" } else { "$gt" };
                            doc! { "$cond": [{ "$or": [missing(&path), { compare: [literal(), &field] }] }, literal(), &field] }.into()
                        }
                        "$currentDate" => match value {
                            Bson::Boolean(true) => Bson::String("$$NOW".into()),
                            Bson::Document(spec) if spec.get_str("$type") == Ok("date") => Bson::String("$$NOW".into()),
                            _ => return Err(refuse("$currentDate", "only dates are simulated (not timestamps)")),
                        },
                        _ => {
                            let items: Bson = match value {
                                Bson::Document(each) if each.keys().any(|key| key.starts_with('$')) => match each.get("$each") {
                                    Some(Bson::Array(items)) if each.len() == 1 => doc! { "$literal": Bson::Array(items.clone()) }.into(),
                                    _ => {
                                        return Err(refuse(
                                            "$push",
                                            "only a value or $each alone is simulated ($slice, $sort, $position are not)",
                                        ))
                                    }
                                },
                                value => Bson::Array(vec![doc! { "$literal": value.clone() }.into()]),
                            };
                            doc! { "$concatArrays": [{ "$ifNull": [&field, []] }, items] }.into()
                        }
                    };
                    set.insert(path, expression);
                }
                stages.push(doc! { "$set": set });
            }
            "$unset" => {
                let removed = fields
                    .keys()
                    .map(|path| claim(preview_path(operator, path)?))
                    .collect::<AppResult<Vec<_>>>()?;
                stages.push(doc! { "$unset": removed });
            }
            "$rename" => {
                for (from, to) in fields {
                    let from = claim(preview_path(operator, from)?)?;
                    let Bson::String(to) = to else {
                        return Err(AppError::new(format!("$rename takes the new name of {from} as text")));
                    };
                    let to = claim(preview_path(operator, to)?)?;
                    // A missing field is not renamed (the target keeps its value).
                    stages.push(doc! { "$set": { &to: { "$cond": [missing(&from), format!("${to}"), format!("${from}")] } } });
                    stages.push(doc! { "$unset": [from] });
                }
            }
            "$addToSet" => return Err(refuse(operator, "the order it keeps is not the aggregation's")),
            "$setOnInsert" => return Err(refuse(operator, "it applies only to upserts, which a bulk update does not do")),
            other => return Err(refuse(other, "it has no exact aggregation equivalent")),
        }
    }
    Ok(stages)
}

fn relaxed(document: Document) -> Json {
    Bson::Document(document).into_relaxed_extjson()
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn statement(coll: &Collection<Document>, method: &str, filter: &Document, update: Option<&Update>) -> String {
    let name = serde_json::to_string(coll.name()).unwrap_or_default();
    match update {
        Some(update) => format!(
            "db.getCollection({name}).{method}({}, {})",
            values::document_text(filter),
            update.text()
        ),
        None => format!("db.getCollection({name}).{method}({})", values::document_text(filter)),
    }
}

pub async fn update_preview(coll: &Collection<Document>, args: &Json, max_time: Duration) -> AppResult<Json> {
    let filter = document_arg(args, "filter")?.unwrap_or_default();
    let update = update_arg(args)?;
    let stages = simulate(&update)?;
    let size = match args.get("sample") {
        None | Some(Json::Null) => DEFAULT_SAMPLE,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| AppError::new("sample must be a whole number"))?
            .clamp(1, MAX_SAMPLE),
    };
    let started = Instant::now();
    let matched = coll.count_documents(filter.clone()).max_time(max_time).await?;
    let before: Vec<Document> = coll
        .find(filter.clone())
        .sort(doc! { ID_FIELD: 1 })
        .limit(i64::try_from(size).unwrap_or(i64::MAX))
        .max_time(max_time)
        .await?
        .try_collect()
        .await?;
    let ids: Vec<Bson> = before.iter().filter_map(|document| document.get(ID_FIELD).cloned()).collect();
    let mut pipeline = vec![doc! { "$match": { ID_FIELD: { "$in": ids } } }, doc! { "$sort": { ID_FIELD: 1 } }];
    pipeline.extend(stages);
    let after: Vec<Document> = if before.is_empty() {
        Vec::new()
    } else {
        coll.aggregate(pipeline).max_time(max_time).await?.try_collect().await?
    };
    if after.len() != before.len() {
        return Err(AppError::new(
            "The preview could not pair the documents before and after the update (someone changed them meanwhile); try again",
        ));
    }
    let sample: Vec<Json> = before
        .into_iter()
        .zip(after)
        .map(|(before, after)| {
            let changed = before != after;
            json!({ "before": relaxed(before), "after": relaxed(after), "changed": changed })
        })
        .collect();
    Ok(json!({
        "matched": matched,
        "sample": sample,
        "elapsedMs": elapsed_ms(started),
        "statement": statement(coll, "updateMany", &filter, Some(&update)),
    }))
}

pub async fn update_many(coll: &Collection<Document>, args: &Json) -> AppResult<Json> {
    let filter = document_arg(args, "filter")?.unwrap_or_default();
    let update = update_arg(args)?;
    let started = Instant::now();
    let result = match update {
        Update::Operators(document) => coll.update_many(filter, document).await?,
        Update::Pipeline(stages) => coll.update_many(filter, stages).await?,
    };
    Ok(json!({ "matched": result.matched_count, "modified": result.modified_count, "elapsedMs": elapsed_ms(started) }))
}

pub async fn delete_many(coll: &Collection<Document>, args: &Json) -> AppResult<Json> {
    let filter = document_arg(args, "filter")?.unwrap_or_default();
    let started = Instant::now();
    let result = coll.delete_many(filter).await?;
    Ok(json!({ "deleted": result.deleted_count, "elapsedMs": elapsed_ms(started) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(value: Json) -> AppResult<Update> {
        update_arg(&json!({ "update": value }))
    }

    fn stages(value: Json) -> AppResult<Vec<Document>> {
        simulate(&update(value)?)
    }

    #[test]
    fn updates_are_operators_or_pipelines() {
        assert!(matches!(update(json!({ "$set": { "a": 1 } })).unwrap(), Update::Operators(_)));
        assert!(matches!(
            update_arg(&json!({ "update": "[ { \"$set\": { \"a\": 1 } } ]" })).unwrap(),
            Update::Pipeline(stages) if stages.len() == 1
        ));
        for bad in [
            json!({}),
            json!({ "a": 1 }),
            json!({ "$set": { "a": 1 }, "b": 2 }),
            json!([]),
            json!([{ "$match": {} }]),
            json!(5),
        ] {
            assert!(update(bad.clone()).is_err(), "{bad}");
        }
        assert!(update_arg(&json!({})).is_err());
    }

    #[test]
    fn empty_filters_are_detected() {
        assert!(filter_matches_everything(&json!({})));
        assert!(filter_matches_everything(&json!({ "filter": "  " })));
        assert!(filter_matches_everything(&json!({ "filter": {} })));
        assert!(filter_matches_everything(&json!({ "filter": "{}" })));
        assert!(filter_matches_everything(&json!({ "filter": "{ nope" })));
        assert!(!filter_matches_everything(&json!({ "filter": { "a": 1 } })));
        assert!(!filter_matches_everything(&json!({ "filter": "{ \"a\": 1 }" })));
    }

    #[test]
    fn trivially_true_filters_match_everything() {
        for all in [
            json!({ "$expr": true }),
            json!({ "$expr": 1 }),
            json!({ "$expr": "yes" }),
            json!({ "$expr": { "$literal": true } }),
            json!({ "_id": { "$exists": true } }),
            json!({ "_id": { "$exists": 1 } }),
            json!({ "$and": [{}] }),
            json!({ "$and": [{ "$expr": true }, { "_id": { "$exists": true } }] }),
            json!({ "$or": [{ "status": "paid" }, {}] }),
            json!({ "$jsonSchema": {} }),
            json!({ "$comment": "all of them" }),
            json!({ "$comment": "x", "$expr": true }),
        ] {
            assert!(matches_everything(&all), "{all}");
            assert!(filter_matches_everything(&json!({ "filter": all.to_string() })), "{all} as text");
        }
        for some in [
            json!({ "$expr": false }),
            json!({ "$expr": 0 }),
            json!({ "$expr": "$flag" }),
            json!({ "$expr": { "$eq": ["$a", 1] } }),
            json!({ "_id": { "$exists": false } }),
            json!({ "_id": { "$exists": true, "$gt": 5 } }),
            json!({ "status": { "$exists": true } }),
            json!({ "$and": [{}, { "a": 1 }] }),
            json!({ "$or": [{ "a": 1 }, { "b": 2 }] }),
            json!({ "$jsonSchema": { "required": ["a"] } }),
            json!({ "$comment": "x", "a": 1 }),
        ] {
            assert!(!matches_everything(&some), "{some}");
        }
    }

    #[test]
    fn operators_become_aggregation_stages() {
        assert_eq!(
            stages(json!({ "$set": { "status": "archived", "meta.by": "$name" } })).unwrap(),
            vec![doc! { "$set": { "status": { "$literal": "archived" }, "meta.by": { "$literal": "$name" } } }]
        );
        assert_eq!(
            stages(json!({ "$unset": { "a": "", "b.c": 1 } })).unwrap(),
            vec![doc! { "$unset": ["a", "b.c"] }]
        );
        assert_eq!(
            stages(json!({ "$inc": { "n": 2 } })).unwrap(),
            vec![doc! { "$set": { "n": { "$add": [{ "$ifNull": ["$n", 0] }, 2] } } }]
        );
        assert_eq!(
            stages(json!({ "$rename": { "old": "new" } })).unwrap(),
            vec![
                doc! { "$set": { "new": { "$cond": [{ "$eq": [{ "$type": "$old" }, "missing"] }, "$new", "$old"] } } },
                doc! { "$unset": ["old"] },
            ]
        );
        assert_eq!(
            stages(json!({ "$currentDate": { "at": true } })).unwrap(),
            vec![doc! { "$set": { "at": "$$NOW" } }]
        );
        assert_eq!(
            stages(json!({ "$push": { "tags": { "$each": ["x", "y"] } } })).unwrap(),
            vec![doc! { "$set": { "tags": { "$concatArrays": [{ "$ifNull": ["$tags", []] }, { "$literal": ["x", "y"] }] } } }]
        );
        assert!(stages(json!({ "$max": { "n": 5 }, "$mul": { "m": 1.5 } })).unwrap().len() == 2);
        let pipeline = json!([{ "$set": { "a": { "$concat": ["$b", "!"] } } }]);
        assert_eq!(stages(pipeline).unwrap().len(), 1);
    }

    #[test]
    fn what_cannot_be_simulated_is_refused() {
        for bad in [
            json!({ "$addToSet": { "tags": "x" } }),
            json!({ "$pull": { "tags": "x" } }),
            json!({ "$pop": { "tags": 1 } }),
            json!({ "$bit": { "n": { "and": 1 } } }),
            json!({ "$setOnInsert": { "a": 1 } }),
            json!({ "$push": { "tags": { "$each": ["x"], "$slice": 3 } } }),
            json!({ "$currentDate": { "at": { "$type": "timestamp" } } }),
            json!({ "$set": { "items.0.qty": 1 } }),
            json!({ "$set": { "items.$.qty": 1 } }),
            json!({ "$set": { "items.$[].qty": 1 } }),
            json!({ "$set": { "_id": 1 } }),
            json!({ "$inc": { "n": "two" } }),
            json!({ "$set": { "a": 1 }, "$unset": { "a.b": "" } }),
            json!({ "$rename": { "a": 5 } }),
        ] {
            assert!(stages(bad.clone()).is_err(), "{bad}");
        }
    }
}
